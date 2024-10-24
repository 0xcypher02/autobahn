use crate::edge::MeteoraCpEdgeIdentifier;
use anchor_lang::{prelude::AccountMeta, Id};
use anchor_spl::{associated_token::get_associated_token_address, token::Token};
use router_lib::dex::{AccountProviderView, SwapInstruction};
use solana_sdk::{account::ReadableAccount, instruction::Instruction, pubkey::Pubkey};

pub fn build_swap_ix(
    id: &MeteoraCpEdgeIdentifier,
    chain_data: &AccountProviderView,
    wallet_pk: &Pubkey,
    in_amount: u64,
    out_amount: u64,
    max_slippage_bps: i32,
) -> anyhow::Result<SwapInstruction> {
    let pool_account = chain_data.account(&id.pool)?;
    let pool = meteora_cpmm_cpi::Pool::deserialize_unchecked(&mut pool_account.account.data())?;

    let amount = in_amount;
    let other_amount_threshold =
        ((out_amount as f64 * (10_000f64 - max_slippage_bps as f64)) / 10_000f64).floor() as u64;

    let (input_token_mint, output_token_mint, admin_token_fee) = if id.is_a_to_b {
        (pool.token_a_mint, pool.token_b_mint, pool.admin_token_a_fee)
    } else {
        (pool.token_b_mint, pool.token_a_mint, pool.admin_token_b_fee)
    };

    let (input_token_account, output_token_account) = (
        get_associated_token_address(wallet_pk, &input_token_mint),
        get_associated_token_address(wallet_pk, &output_token_mint),
    );

    let instruction = meteora_cpmm_cpi::encode_swap(amount, other_amount_threshold);

    let (a_vault, b_vault) = {
        let a_vault_account = chain_data.account(&pool.a_vault)?;
        let b_vault_account = chain_data.account(&pool.b_vault)?;
        let a_vault =
            meteora_vault_cpi::Vault::deserialize_unchecked(&mut a_vault_account.account.data())?;
        let b_vault =
            meteora_vault_cpi::Vault::deserialize_unchecked(&mut b_vault_account.account.data())?;
        (a_vault, b_vault)
    };

    let accounts = vec![
        AccountMeta::new_readonly(meteora_cpmm_cpi::id(), false),
        AccountMeta::new(id.pool, false),
        AccountMeta::new(input_token_account, false),
        AccountMeta::new(output_token_account, false),
        AccountMeta::new(pool.a_vault, false),
        AccountMeta::new(pool.b_vault, false),
        AccountMeta::new(a_vault.token_vault, false),
        AccountMeta::new(b_vault.token_vault, false),
        AccountMeta::new(a_vault.lp_mint, false),
        AccountMeta::new(b_vault.lp_mint, false),
        AccountMeta::new(pool.a_vault_lp, false),
        AccountMeta::new(pool.b_vault_lp, false),
        AccountMeta::new(admin_token_fee, false),
        AccountMeta::new(*wallet_pk, true),
        AccountMeta::new_readonly(meteora_vault_cpi::id(), false),
        AccountMeta::new_readonly(Token::id(), false),
    ];

    let result = SwapInstruction {
        instruction: Instruction {
            program_id: meteora_cpmm_cpi::id(),
            accounts,
            data: instruction,
        },
        out_pubkey: output_token_account,
        out_mint: output_token_mint,
        in_amount_offset: 8,
        cu_estimate: Some(40_000),
    };

    Ok(result)
}
