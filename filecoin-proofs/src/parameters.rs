use anyhow::{ensure, Result};
use storage_proofs_core::{api_version::ApiFeature, proof::ProofScheme};
use storage_proofs_porep::stacked::{self, LayerChallenges, StackedDrg};
use storage_proofs_post::fallback::{self, FallbackPoSt};

use crate::{
    constants::{DefaultPieceHasher, DRG_DEGREE, EXP_DEGREE, LAYERS},
    types::{MerkleTreeTrait, PoRepConfig, PoStConfig},
    POREP_MINIMUM_CHALLENGES,
};

type WinningPostSetupParams = fallback::SetupParams;
pub type WinningPostPublicParams = fallback::PublicParams;

type WindowPostSetupParams = fallback::SetupParams;
pub type WindowPostPublicParams = fallback::PublicParams;

pub fn public_params<Tree: 'static + MerkleTreeTrait>(
    porep_config: &PoRepConfig,
) -> Result<stacked::PublicParams<Tree>> {
    StackedDrg::<Tree, DefaultPieceHasher>::setup(&setup_params(porep_config)?)
}

pub fn winning_post_public_params<Tree: 'static + MerkleTreeTrait>(
    post_config: &PoStConfig,
) -> Result<WinningPostPublicParams> {
    FallbackPoSt::<Tree>::setup(&winning_post_setup_params(post_config)?)
}

pub fn winning_post_setup_params(post_config: &PoStConfig) -> Result<WinningPostSetupParams> {
    ensure!(
        post_config.challenge_count % post_config.sector_count == 0,
        "sector count must divide challenge count"
    );

    let param_sector_count = post_config.challenge_count / post_config.sector_count;
    let param_challenge_count = post_config.challenge_count / param_sector_count;

    ensure!(
        param_sector_count * param_challenge_count == post_config.challenge_count,
        "invald parameters calculated {} * {} != {}",
        param_sector_count,
        param_challenge_count,
        post_config.challenge_count
    );

    Ok(fallback::SetupParams {
        sector_size: post_config.padded_sector_size().into(),
        challenge_count: param_challenge_count,
        sector_count: param_sector_count,
        api_version: post_config.api_version,
    })
}

pub fn window_post_public_params<Tree: 'static + MerkleTreeTrait>(
    post_config: &PoStConfig,
) -> Result<WindowPostPublicParams> {
    FallbackPoSt::<Tree>::setup(&window_post_setup_params(post_config))
}

pub fn window_post_setup_params(post_config: &PoStConfig) -> WindowPostSetupParams {
    fallback::SetupParams {
        sector_size: post_config.padded_sector_size().into(),
        challenge_count: post_config.challenge_count,
        sector_count: post_config.sector_count,
        api_version: post_config.api_version,
    }
}

pub fn setup_params(porep_config: &PoRepConfig) -> Result<stacked::SetupParams> {
    let use_synthetic = porep_config.feature_enabled(ApiFeature::SyntheticPoRep);
    let sector_bytes = porep_config.padded_bytes_amount();
    let layer_challenges = select_challenges(
        usize::from(porep_config.partitions),
        POREP_MINIMUM_CHALLENGES.from_sector_size(u64::from(sector_bytes)),
        *LAYERS
            .read()
            .expect("LAYERS poisoned")
            .get(&u64::from(sector_bytes))
            .expect("unknown sector size"),
        use_synthetic,
    );
    let sector_bytes = u64::from(sector_bytes);

    ensure!(
        sector_bytes % 32 == 0,
        "sector_bytes ({}) must be a multiple of 32",
        sector_bytes,
    );

    let nodes = (sector_bytes / 32) as usize;
    let degree = DRG_DEGREE;
    let expansion_degree = EXP_DEGREE;

    Ok(stacked::SetupParams {
        nodes,
        degree,
        expansion_degree,
        porep_id: porep_config.porep_id,
        layer_challenges,
        api_version: porep_config.api_version,
        api_features: porep_config.api_features.clone(),
    })
}

fn select_challenges(
    partitions: usize,
    minimum_total_challenges: usize,
    layers: usize,
    use_synthetic: bool,
) -> LayerChallenges {
    let mut count = 1;
    let mut guess = LayerChallenges::new(layers, count);
    while partitions * guess.challenges_count_all() < minimum_total_challenges {
        count += 1;
        guess = LayerChallenges::new(layers, count);
    }

    guess.use_synthetic = use_synthetic;
    guess
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{DefaultOctLCTree, PoRepProofPartitions, PoStType};

    #[test]
    fn partition_layer_challenges_test() {
        let f = |partitions| select_challenges(partitions, 12, 11, false).challenges_count_all();
        // Update to ensure all supported PoRepProofPartitions options are represented here.
        assert_eq!(6, f(usize::from(PoRepProofPartitions(2))));

        assert_eq!(12, f(1));
        assert_eq!(6, f(2));
        assert_eq!(3, f(4));
    }

    #[test]
    fn test_winning_post_params() {
        use storage_proofs_core::api_version::ApiVersion;

        let config = PoStConfig {
            typ: PoStType::Winning,
            priority: false,
            challenge_count: 66,
            sector_count: 1,
            sector_size: 2048u64.into(),
            api_version: ApiVersion::V1_2_0,
        };

        let params =
            winning_post_public_params::<DefaultOctLCTree>(&config).expect("failed to get params");
        assert_eq!(params.sector_count, 66);
        assert_eq!(params.challenge_count, 1);
        assert_eq!(params.sector_size, 2048);
    }
}
