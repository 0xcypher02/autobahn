use std::any::Any;

use anchor_spl::token::spl_token::state::{Account, Mint};
use raydium_cp_swap::utils::CheckedCeilDiv;
use router_lib::dex::{DexEdge, DexEdgeIdentifier, Quote};
use solana_sdk::pubkey::Pubkey;

pub struct MeteoraCpEdgeIdentifier {
    pub pool: Pubkey,
    pub a_mint: Pubkey,
    pub b_mint: Pubkey,
    pub is_a_to_b: bool,
}

impl DexEdgeIdentifier for MeteoraCpEdgeIdentifier {
    fn key(&self) -> Pubkey {
        self.pool
    }

    fn desc(&self) -> String {
        format!("MeteoraCp_{}", self.pool)
    }

    fn input_mint(&self) -> Pubkey {
        if self.is_a_to_b {
            self.a_mint
        } else {
            self.b_mint
        }
    }

    fn output_mint(&self) -> Pubkey {
        if self.is_a_to_b {
            self.b_mint
        } else {
            self.a_mint
        }
    }

    fn accounts_needed(&self) -> usize {
        6
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct MeteoraCpEdge {
    pub pool: meteora_cpmm_cpi::Pool,
    pub a_vault: meteora_vault_cpi::Vault,
    pub b_vault: meteora_vault_cpi::Vault,
    pub a_vault_token: Account,
    pub b_vault_token: Account,
    pub a_vault_lp_token: Account,
    pub b_vault_lp_token: Account,
    pub a_vault_lp_mint: Mint,
    pub b_vault_lp_mint: Mint,
}

impl DexEdge for MeteoraCpEdge {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl MeteoraCpEdge {
    pub fn quote_exact_in(
        &self,
        current_time: u64,
        amount_in: u64,
        is_a_to_b: bool,
    ) -> Option<Quote> {
        let token_a_amount = self.a_vault.get_amount_by_share(
            current_time,
            self.a_vault_lp_token.amount,
            self.a_vault_lp_mint.supply,
        )?;

        let token_b_amount = self.b_vault.get_amount_by_share(
            current_time,
            self.b_vault_lp_token.amount,
            self.b_vault_lp_mint.supply,
        )?;

        let (
            mut in_vault,
            out_vault,
            in_vault_lp,
            in_vault_lp_mint,
            out_vault_lp_mint,
            out_vault_token_account,
            in_token_total_amount,
            out_token_total_amount,
            in_mint,
            _out_mint,
        ) = if is_a_to_b {
            (
                meteora_vault_cpi::Vault::clone(&self.a_vault),
                &self.b_vault,
                &self.a_vault_lp_token,
                &self.a_vault_lp_mint,
                &self.b_vault_lp_mint,
                &self.b_vault_token,
                token_a_amount,
                token_b_amount,
                self.pool.token_a_mint,
                self.pool.token_b_mint,
            )
        } else {
            (
                meteora_vault_cpi::Vault::clone(&self.b_vault),
                &self.a_vault,
                &self.b_vault_lp_token,
                &self.b_vault_lp_mint,
                &self.a_vault_lp_mint,
                &self.a_vault_token,
                token_b_amount,
                token_a_amount,
                self.pool.token_b_mint,
                self.pool.token_a_mint,
            )
        };

        let trade_fee = self.pool.fees.trading_fee(amount_in.into())?;
        let owner_fee = self.pool.fees.owner_trading_fee(amount_in.into())?;

        let in_amount_after_owner_fee = amount_in.checked_sub(owner_fee.try_into().ok()?)?;

        let before_in_token_total_amount = in_token_total_amount;

        let in_lp = in_vault.get_unmint_amount(
            current_time,
            in_amount_after_owner_fee,
            in_vault_lp_mint.supply,
        )?;

        in_vault.total_amount = in_vault
            .total_amount
            .checked_add(in_amount_after_owner_fee)?;

        let after_in_token_total_amount = in_vault.get_amount_by_share(
            current_time,
            in_lp.checked_add(in_vault_lp.amount)?,
            in_vault_lp_mint.supply.checked_add(in_lp)?,
        )?;

        let actual_in_amount =
            after_in_token_total_amount.checked_sub(before_in_token_total_amount)?;

        let actual_in_amount_after_fee =
            actual_in_amount.checked_sub(trade_fee.try_into().ok()?)?;

        let destination_amount_swapped = {
            let source_amount: u128 = actual_in_amount_after_fee.into();
            let swap_source_amount: u128 = in_token_total_amount.into();
            let swap_destination_amount: u128 = out_token_total_amount.into();

            let destination_amount_swapped = {
                let invariant = swap_source_amount.checked_mul(swap_destination_amount)?;

                let new_swap_source_amount = swap_source_amount.checked_add(source_amount)?;
                let (new_swap_destination_amount, _) =
                    invariant.checked_ceil_div(new_swap_source_amount)?;

                let destination_amount_swapped = swap_destination_amount
                    .checked_sub(new_swap_destination_amount)
                    .filter(|out| out != &0)?;

                Some(destination_amount_swapped)
            }?;

            Some(destination_amount_swapped)
        }?;

        let out_vault_lp = out_vault.get_unmint_amount(
            current_time,
            destination_amount_swapped.try_into().unwrap(),
            out_vault_lp_mint.supply,
        )?;

        let out_amount =
            out_vault.get_amount_by_share(current_time, out_vault_lp, out_vault_lp_mint.supply)?;

        // TX would revert due to insufficient vault reserves.
        if out_amount > out_vault_token_account.amount {
            return None;
        }

        Some(Quote {
            in_amount: amount_in,
            out_amount,
            fee_amount: trade_fee.try_into().unwrap(),
            fee_mint: in_mint,
        })
    }
}
