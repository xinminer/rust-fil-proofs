use std::collections::BTreeMap;

use anyhow::{anyhow, ensure, Context, Result};
use filecoin_hashers::Hasher;
use log::{debug, info};
use storage_proofs_core::{merkle::MerkleTreeTrait, proof::ProofScheme, sector::SectorId};
use storage_proofs_post::fallback::{
    self, generate_leaf_challenge, get_challenge_index, FallbackPoSt, SectorProof,
};

use crate::{
    api::as_safe_commitment,
    types::{
        ChallengeSeed, FallbackPoStSectorProof, PoStConfig, PrivateReplicaInfo, ProverId,
        VanillaProof,
    },
    PartitionSnarkProof, PoStType, SnarkProof, SINGLE_PARTITION_PROOF_LEN,
};

/// Generates the challenges per SectorId required for either a Window
/// proof-of-spacetime or a Winning proof-of-spacetime.
pub fn generate_fallback_sector_challenges<Tree: 'static + MerkleTreeTrait>(
    post_config: &PoStConfig,
    randomness: &ChallengeSeed,
    pub_sectors: &[SectorId],
    _prover_id: ProverId,
) -> Result<BTreeMap<SectorId, Vec<u64>>> {
    info!("generate_sector_challenges:start");
    ensure!(
        post_config.typ == PoStType::Window || post_config.typ == PoStType::Winning,
        "invalid post config type"
    );

    let randomness_safe: <Tree::Hasher as Hasher>::Domain =
        as_safe_commitment(randomness, "randomness")?;

    let public_params = fallback::PublicParams {
        sector_size: u64::from(post_config.sector_size),
        challenge_count: post_config.challenge_count,
        sector_count: post_config.sector_count,
        api_version: post_config.api_version,
    };

    let mut sector_challenges: BTreeMap<SectorId, Vec<u64>> = BTreeMap::new();

    let num_sectors_per_chunk = post_config.sector_count;
    let partitions = match post_config.typ {
        PoStType::Window => {
            get_partitions_for_window_post(pub_sectors.len(), post_config).unwrap_or(1)
        }
        PoStType::Winning => 1,
    };

    for partition_index in 0..partitions {
        let sectors = pub_sectors
            .chunks(num_sectors_per_chunk)
            .nth(partition_index)
            .ok_or_else(|| anyhow!("invalid number of sectors/partition index"))?;

        for (i, sector) in sectors.iter().enumerate() {
            let mut challenges = Vec::new();

            for n in 0..post_config.challenge_count {
                let sector_index = match post_config.typ {
                    PoStType::Window => partition_index * num_sectors_per_chunk + i,
                    PoStType::Winning => partition_index * post_config.sector_count + i,
                };
                let challenge_index = get_challenge_index(
                    post_config.api_version,
                    sector_index,
                    post_config.challenge_count,
                    n,
                );
                let challenged_leaf = generate_leaf_challenge(
                    &public_params,
                    randomness_safe,
                    u64::from(*sector),
                    challenge_index,
                );
                challenges.push(challenged_leaf);
            }

            sector_challenges.insert(*sector, challenges);
        }
    }

    info!("generate_sector_challenges:finish");

    Ok(sector_challenges)
}

/// Generates a single vanilla proof required for either Window proof-of-spacetime
/// or Winning proof-of-spacetime.
pub fn generate_single_vanilla_proof<Tree: 'static + MerkleTreeTrait>(
    post_config: &PoStConfig,
    sector_id: SectorId,
    replica: &PrivateReplicaInfo<Tree>,
    challenges: &[u64],
) -> Result<FallbackPoStSectorProof<Tree>> {
    info!("generate_single_vanilla_proof:start: {:?}", sector_id);

    let tree = &replica
        .merkle_tree(post_config.sector_size)
        .with_context(|| {
            format!(
                "generate_single_vanilla_proof: merkle_tree failed: {:?}",
                sector_id
            )
        })?;
    let comm_r = replica.safe_comm_r().with_context(|| {
        format!(
            "generate_single_vanilla_poof: safe_comm_r failed: {:?}",
            sector_id
        )
    })?;
    let comm_c = replica.safe_comm_c();
    let comm_r_last = replica.safe_comm_r_last();

    let priv_sectors = vec![fallback::PrivateSector {
        tree,
        comm_c,
        comm_r_last,
    }];

    let priv_inputs = fallback::PrivateInputs::<Tree> {
        sectors: &priv_sectors,
    };

    let vanilla_proof =
        fallback::vanilla_proof(sector_id, &priv_inputs, challenges).with_context(|| {
            format!(
                "generate_single_vanilla_proof: vanilla_proof failed: {:?}",
                sector_id
            )
        })?;

    info!("generate_single_vanilla_proof:finish: {:?}", sector_id);

    Ok(FallbackPoStSectorProof {
        sector_id,
        comm_r,
        vanilla_proof,
    })
}

// Partition a flat vector of vanilla sector proofs.  The post_config
// (PoSt) type is required in order to determine the proper shape of
// the returned partitioned proofs.
pub fn partition_vanilla_proofs<Tree: MerkleTreeTrait>(
    post_config: &PoStConfig,
    pub_params: &fallback::PublicParams,
    pub_inputs: &fallback::PublicInputs<<Tree::Hasher as Hasher>::Domain>,
    partition_count: usize,
    vanilla_proofs: &[FallbackPoStSectorProof<Tree>],
) -> Result<Vec<VanillaProof<Tree>>> {
    info!("partition_vanilla_proofs:start");
    ensure!(
        post_config.typ == PoStType::Window || post_config.typ == PoStType::Winning,
        "invalid post config type"
    );

    let num_sectors_per_chunk = pub_params.sector_count;
    let num_sectors = pub_inputs.sectors.len();

    ensure!(
        num_sectors <= partition_count * num_sectors_per_chunk,
        "cannot prove the provided number of sectors: {} > {} * {}",
        num_sectors,
        partition_count,
        num_sectors_per_chunk,
    );

    let mut partition_proofs = Vec::new();

    // Note that the partition proofs returned are shaped differently
    // based on which type of PoSt is being considered.
    match post_config.typ {
        PoStType::Window => {
            for (j, sectors_chunk) in pub_inputs.sectors.chunks(num_sectors_per_chunk).enumerate() {
                let proof = single_partition_vanilla_proofs(
                    post_config,
                    pub_params,
                    &fallback::PublicInputs {
                        randomness: pub_inputs.randomness,
                        prover_id: pub_inputs.prover_id,
                        sectors: sectors_chunk.to_vec(),
                        k: Some(j),
                    },
                    vanilla_proofs,
                )?;
                partition_proofs.push(proof);
            }
        }
        PoStType::Winning => {
            for (j, sectors_chunk) in vanilla_proofs.chunks(num_sectors_per_chunk).enumerate() {
                let proof = single_partition_vanilla_proofs(
                    post_config,
                    pub_params,
                    &fallback::FallbackPoSt::<Tree>::with_partition(pub_inputs.clone(), Some(j)),
                    sectors_chunk,
                )?;
                partition_proofs.push(proof);
            }
        }
    }

    info!("partition_vanilla_proofs:finish");

    ensure!(
        FallbackPoSt::<Tree>::verify_all_partitions(pub_params, pub_inputs, &partition_proofs)?,
        "partitioned vanilla proofs failed to verify"
    );

    Ok(partition_proofs)
}

pub(crate) fn get_partitions_for_window_post(
    total_sector_count: usize,
    post_config: &PoStConfig,
) -> Option<usize> {
    let partitions = (total_sector_count as f32 / post_config.sector_count as f32).ceil() as usize;

    if partitions > 1 {
        Some(partitions)
    } else {
        None
    }
}

pub fn single_partition_vanilla_proofs<Tree: MerkleTreeTrait>(
    post_config: &PoStConfig,
    pub_params: &fallback::PublicParams,
    pub_inputs: &fallback::PublicInputs<<Tree::Hasher as Hasher>::Domain>,
    vanilla_proofs: &[FallbackPoStSectorProof<Tree>],
) -> Result<VanillaProof<Tree>> {
    info!("single_partition_vanilla_proofs:start");
    ensure!(pub_inputs.k.is_some(), "must have a partition index");
    let partition_index = pub_inputs.k.expect("prechecked");

    debug!("processing partition: {}", partition_index);
    ensure!(
        post_config.typ == PoStType::Window || post_config.typ == PoStType::Winning,
        "invalid post config type"
    );

    let num_sectors_per_chunk = pub_params.sector_count;
    let num_sectors = pub_inputs.sectors.len();
    ensure!(
        num_sectors <= num_sectors_per_chunk,
        "can only prove a single partition"
    );

    // Note that the partition proofs returned are shaped differently
    // based on which type of PoSt is being considered.
    let partition_proof = match post_config.typ {
        PoStType::Window => {
            let sectors_chunk = &pub_inputs.sectors;

            let mut sector_proofs = Vec::with_capacity(num_sectors_per_chunk);

            for pub_sector in sectors_chunk.iter() {
                let cur_proof = vanilla_proofs
                    .iter()
                    .find(|&proof| proof.sector_id == pub_sector.id)
                    .expect("failed to locate sector proof");

                // Note: Window post requires all inclusion proofs (based on the challenge
                // count per sector) per sector proof.
                sector_proofs.extend(cur_proof.vanilla_proof.sectors.clone());
            }

            // If there were less than the required number of sectors provided, we duplicate the last one
            // to pad the proof out, such that it works in the circuit part.
            while sector_proofs.len() < num_sectors_per_chunk {
                sector_proofs.push(sector_proofs[sector_proofs.len() - 1].clone());
            }

            fallback::Proof::<<Tree as MerkleTreeTrait>::Proof> {
                sectors: sector_proofs,
            }
        }
        PoStType::Winning => {
            let sectors_chunk = vanilla_proofs;
            // Sanity check incoming structure
            ensure!(
                sectors_chunk.len() == 1,
                "Invalid sector chunk for Winning PoSt"
            );
            ensure!(
                sectors_chunk[0].vanilla_proof.sectors.len() == 1,
                "Invalid sector count for Winning PoSt chunk"
            );

            // Winning post sector_count is winning post challenges per sector
            ensure!(
                post_config.sector_count
                    == sectors_chunk[partition_index].vanilla_proof.sectors.len(),
                "invalid number of sector proofs for Winning PoSt"
            );

            let mut sector_proofs = Vec::with_capacity(post_config.challenge_count);
            let cur_sector_proof = &sectors_chunk[0].vanilla_proof.sectors[0];

            // Unroll inclusions proofs from the single provided sector_proof (per partition)
            // into individual sector proofs, required for winning post.
            for cur_inclusion_proof in cur_sector_proof.inclusion_proofs() {
                sector_proofs.push(SectorProof {
                    inclusion_proofs: vec![cur_inclusion_proof.clone()],
                    comm_c: cur_sector_proof.comm_c,
                    comm_r_last: cur_sector_proof.comm_r_last,
                });
            }

            // If there were less than the required number of sectors provided, we duplicate the last one
            // to pad the proof out, such that it works in the circuit part.
            while sector_proofs.len() < num_sectors_per_chunk {
                sector_proofs.push(sector_proofs[sector_proofs.len() - 1].clone());
            }

            // Winning post Challenge count is the total winning post challenges
            ensure!(
                sector_proofs.len() == post_config.challenge_count,
                "invalid number of partition proofs based on Winning PoSt challenges"
            );

            fallback::Proof::<<Tree as MerkleTreeTrait>::Proof> {
                sectors: sector_proofs,
            }
        }
    };

    info!("single_partition_vanilla_proofs:finish");

    ensure!(
        FallbackPoSt::<Tree>::verify(pub_params, pub_inputs, &partition_proof)?,
        "partitioned vanilla proofs failed to verify"
    );

    Ok(partition_proof)
}

pub fn merge_window_post_partition_proofs(
    mut proofs: Vec<PartitionSnarkProof>,
) -> Result<SnarkProof> {
    let mut proof = Vec::with_capacity(proofs.len() * SINGLE_PARTITION_PROOF_LEN);
    for p in proofs.iter_mut() {
        proof.append(&mut p.0);
    }

    Ok(proof)
}

pub fn get_num_partition_for_fallback_post(config: &PoStConfig, num_sectors: usize) -> usize {
    match config.typ {
        PoStType::Window => {
            let partitions = (num_sectors as f32 / config.sector_count as f32).ceil() as usize;
            if partitions > 1 {
                partitions
            } else {
                1
            }
        }
        PoStType::Winning => 1,
    }
}
