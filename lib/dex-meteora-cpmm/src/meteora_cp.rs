use crate::edge::{MeteoraCpEdge, MeteoraCpEdgeIdentifier};
use crate::meteora_cp_ix_builder;
use anchor_lang::Discriminator;
use anchor_spl::token::spl_token;
use anyhow::Context;
use async_trait::async_trait;
use itertools::Itertools;
use meteora_cpmm_cpi::Pool;
use router_feed_lib::router_rpc_client::{RouterRpcClient, RouterRpcClientTrait};
use router_lib::dex::{
    AccountProviderView, DexEdge, DexEdgeIdentifier, DexInterface, DexSubscriptionMode,
    MixedDexSubscription, Quote, SwapInstruction,
};
use router_lib::utils;
use solana_account_decoder::UiAccountEncoding;
use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_client::rpc_filter::{Memcmp, RpcFilterType};
use solana_sdk::account::ReadableAccount;
use solana_sdk::clock::Clock;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::program_pack::Pack;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::sysvar::SysvarId;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::u64;

pub struct MeteoraCpDex {
    pub edges: HashMap<Pubkey, Vec<Arc<dyn DexEdgeIdentifier>>>,
    subscription_accounts: HashSet<Pubkey>,
}

#[async_trait]
impl DexInterface for MeteoraCpDex {
    async fn initialize(
        rpc: &mut RouterRpcClient,
        _options: HashMap<String, String>,
    ) -> anyhow::Result<Arc<dyn DexInterface>>
    where
        Self: Sized,
    {
        let pools: Vec<(Pubkey, Pool)> = fetch_meteora_account(rpc, meteora_cpmm_cpi::id())
            .await?
            .into_iter()
            .filter(|(_, pool)| {
                pool.enabled
                    && matches!(
                        pool.curve_type,
                        meteora_cpmm_cpi::CurveType::ConstantProduct
                    )
            })
            .collect();

        let vaults = rpc
            .get_multiple_accounts(
                &pools
                    .iter()
                    .flat_map(|x| [x.1.a_vault, x.1.b_vault])
                    .collect::<HashSet<_>>(),
            )
            .await?
            .into_iter()
            .flat_map(|(pk, account)| {
                match meteora_vault_cpi::Vault::deserialize_unchecked(&account.data) {
                    Ok(vault) => Some((pk, vault)),
                    Err(_) => None,
                }
            })
            .collect::<HashMap<_, _>>();

        let edge_pairs = pools
            .iter()
            .map(|(pool_pk, pool)| {
                (
                    Arc::new(MeteoraCpEdgeIdentifier {
                        pool: *pool_pk,
                        a_mint: pool.token_a_mint,
                        b_mint: pool.token_b_mint,
                        is_a_to_b: true,
                    }),
                    Arc::new(MeteoraCpEdgeIdentifier {
                        pool: *pool_pk,
                        a_mint: pool.token_a_mint,
                        b_mint: pool.token_b_mint,
                        is_a_to_b: false,
                    }),
                )
            })
            .collect_vec();

        let mut subscription_accounts = HashSet::new();
        subscription_accounts.insert(Clock::id());

        let edges_per_pk = {
            let mut map = HashMap::new();

            for ((pool_pk, pool), (edge_a_to_b, edge_b_to_a)) in pools.iter().zip(edge_pairs.iter())
            {
                let entry = vec![
                    edge_a_to_b.clone() as Arc<dyn DexEdgeIdentifier>,
                    edge_b_to_a.clone(),
                ];

                let a_vault = vaults.get(&pool.a_vault).unwrap();
                let b_vault = vaults.get(&pool.b_vault).unwrap();

                utils::insert_or_extend(&mut map, pool_pk, &entry);
                utils::insert_or_extend(&mut map, &pool.a_vault, &entry);
                utils::insert_or_extend(&mut map, &pool.b_vault, &entry);

                utils::insert_or_extend(&mut map, &a_vault.token_vault, &entry);
                subscription_accounts.insert(a_vault.token_vault);
                utils::insert_or_extend(&mut map, &b_vault.token_vault, &entry);
                subscription_accounts.insert(b_vault.token_vault);
                utils::insert_or_extend(&mut map, &pool.a_vault_lp, &entry);
                subscription_accounts.insert(pool.a_vault_lp);
                utils::insert_or_extend(&mut map, &pool.b_vault_lp, &entry);
                subscription_accounts.insert(pool.b_vault_lp);
                utils::insert_or_extend(&mut map, &a_vault.lp_mint, &entry);
                subscription_accounts.insert(a_vault.lp_mint);
                utils::insert_or_extend(&mut map, &b_vault.lp_mint, &entry);
                subscription_accounts.insert(b_vault.lp_mint);
            }
            map
        };

        Ok(Arc::new(MeteoraCpDex {
            edges: edges_per_pk,
            subscription_accounts,
        }))
    }

    fn name(&self) -> String {
        "MeteoraCp".to_string()
    }

    fn subscription_mode(&self) -> DexSubscriptionMode {
        DexSubscriptionMode::Mixed(MixedDexSubscription {
            accounts: self.subscription_accounts.clone(),
            programs: HashSet::from([meteora_cpmm_cpi::id(), meteora_vault_cpi::id()]),
            token_accounts_for_owner: Default::default(),
        })
    }

    fn program_ids(&self) -> HashSet<Pubkey> {
        [meteora_cpmm_cpi::id(), meteora_vault_cpi::id()]
            .into_iter()
            .collect()
    }

    fn edges_per_pk(&self) -> HashMap<Pubkey, Vec<Arc<dyn DexEdgeIdentifier>>> {
        self.edges.clone()
    }

    fn load(
        &self,
        id: &Arc<dyn DexEdgeIdentifier>,
        chain_data: &AccountProviderView,
    ) -> anyhow::Result<Arc<dyn DexEdge>> {
        let id = id
            .as_any()
            .downcast_ref::<MeteoraCpEdgeIdentifier>()
            .unwrap();

        let pool_account = chain_data.account(&id.pool)?;
        let pool = meteora_cpmm_cpi::Pool::deserialize_unchecked(&mut pool_account.account.data())?;

        let a_vault_account = chain_data.account(&pool.a_vault)?;
        let a_vault =
            meteora_vault_cpi::Vault::deserialize_unchecked(&mut a_vault_account.account.data())?;

        let b_vault_account = chain_data.account(&pool.b_vault)?;
        let b_vault =
            meteora_vault_cpi::Vault::deserialize_unchecked(&mut b_vault_account.account.data())?;

        let a_vault_lp_account = chain_data.account(&pool.a_vault_lp)?;
        let a_vault_lp_token =
            spl_token::state::Account::unpack(&a_vault_lp_account.account.data())?;

        let b_vault_lp_account = chain_data.account(&pool.b_vault_lp)?;
        let b_vault_lp_token =
            spl_token::state::Account::unpack(&b_vault_lp_account.account.data())?;

        let a_vault_token_account = chain_data.account(&a_vault.token_vault)?;
        let a_vault_token =
            spl_token::state::Account::unpack(&a_vault_token_account.account.data())?;

        let b_vault_token_account = chain_data.account(&b_vault.token_vault)?;
        let b_vault_token =
            spl_token::state::Account::unpack(&b_vault_token_account.account.data())?;

        let a_vault_lp_mint_account = chain_data.account(&a_vault.lp_mint)?;
        let a_vault_lp_mint =
            spl_token::state::Mint::unpack(&a_vault_lp_mint_account.account.data())?;

        let b_vault_lp_mint_account = chain_data.account(&b_vault.lp_mint)?;
        let b_vault_lp_mint =
            spl_token::state::Mint::unpack(&b_vault_lp_mint_account.account.data())?;

        Ok(Arc::new(MeteoraCpEdge {
            pool,
            a_vault,
            b_vault,
            a_vault_token,
            b_vault_token,
            a_vault_lp_token,
            b_vault_lp_token,
            a_vault_lp_mint,
            b_vault_lp_mint,
        }))
    }

    fn quote(
        &self,
        id: &Arc<dyn DexEdgeIdentifier>,
        edge: &Arc<dyn DexEdge>,
        chain_data: &AccountProviderView,
        in_amount: u64,
    ) -> anyhow::Result<Quote> {
        let id = id
            .as_any()
            .downcast_ref::<MeteoraCpEdgeIdentifier>()
            .unwrap();
        let edge = edge.as_any().downcast_ref::<MeteoraCpEdge>().unwrap();

        if !edge.pool.enabled {
            return Ok(Quote {
                in_amount: 0,
                out_amount: 0,
                fee_amount: 0,
                fee_mint: edge.pool.token_a_mint,
            });
        }

        if !matches!(
            edge.pool.curve_type,
            meteora_cpmm_cpi::CurveType::ConstantProduct
        ) {
            // TODO: Support other curve types
            return Ok(Quote {
                in_amount: 0,
                out_amount: 0,
                fee_amount: 0,
                fee_mint: edge.pool.token_a_mint,
            });
        }

        let clock = chain_data.account(&Clock::id()).context("read clock")?;
        let current_time = clock.account.deserialize_data::<Clock>()?.unix_timestamp as u64;

        if let Some(quote) = edge.quote_exact_in(current_time, in_amount, id.is_a_to_b) {
            return Ok(quote);
        } else {
            return Ok(Quote {
                in_amount: 0,
                out_amount: 0,
                fee_amount: 0,
                fee_mint: edge.pool.token_a_mint,
            });
        }
    }

    fn build_swap_ix(
        &self,
        id: &Arc<dyn DexEdgeIdentifier>,
        chain_data: &AccountProviderView,
        wallet_pk: &Pubkey,
        in_amount: u64,
        out_amount: u64,
        max_slippage_bps: i32,
    ) -> anyhow::Result<SwapInstruction> {
        let id = id
            .as_any()
            .downcast_ref::<MeteoraCpEdgeIdentifier>()
            .unwrap();
        meteora_cp_ix_builder::build_swap_ix(
            id,
            chain_data,
            wallet_pk,
            in_amount,
            out_amount,
            max_slippage_bps,
        )
    }

    fn supports_exact_out(&self, _id: &Arc<dyn DexEdgeIdentifier>) -> bool {
        false
    }

    fn quote_exact_out(
        &self,
        _id: &Arc<dyn DexEdgeIdentifier>,
        _edge: &Arc<dyn DexEdge>,
        _chain_data: &AccountProviderView,
        _out_amount: u64,
    ) -> anyhow::Result<Quote> {
        unreachable!("quote_exact_out not supported");
    }
}

async fn fetch_meteora_account(
    rpc: &mut RouterRpcClient,
    program_id: Pubkey,
) -> anyhow::Result<Vec<(Pubkey, Pool)>> {
    let config = RpcProgramAccountsConfig {
        filters: Some(vec![RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
            0,
            Pool::DISCRIMINATOR.to_vec(),
        ))]),
        account_config: RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            commitment: Some(CommitmentConfig::finalized()),
            ..Default::default()
        },
        ..Default::default()
    };

    let snapshot = rpc
        .get_program_accounts_with_config(&program_id, config)
        .await?;

    let result = snapshot
        .iter()
        .map(|account| {
            let pool = Pool::deserialize_unchecked(&mut account.data.as_slice()).unwrap();
            (account.pubkey, pool)
        })
        .collect_vec();

    Ok(result)
}