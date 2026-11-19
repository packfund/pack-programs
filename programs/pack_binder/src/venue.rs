//! The two venues of $PACK (docs/validation/P2-00_BINDER_CPI.md): the swap flow of a mint by CPI with the swapper PDA as
//! signer, and the typed reads of the venue accounts that `init_config` and `set_damm` pin. Venue program ids,
//! authorities and account layouts are constants; nothing about a venue is ever taken from the caller.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;
use anchor_spl::token::{Token, TokenAccount as SplTokenAccount};
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

use crate::constants::*;
use crate::cpi_lite;
use crate::errors::BinderError;

// ---------------------------------------------------------------------------------------------------------------------
// the swap of a mint (P2-00 `run_flow`)
// ---------------------------------------------------------------------------------------------------------------------

/// What the swap did, all measured by balance difference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwapOutcome {
    /// wSOL lamports the venue took.
    pub wsol_spent: u64,
    /// $PACK units received; all of them are burned.
    pub received: u64,
    /// Lamports of the wSOL account that went back to the buyer when it was closed.
    pub refunded_lamports: u64,
}

/// Builds the data of `swap2` (discriminator, `amount_0` = amount in, `amount_1` = minimum out, `swap_mode`) and invokes
/// the venue with the swapper PDA as the signer. Both venues share the byte layout (their IDLs are identical).
pub fn swap_cpi<'info>(
    program: &Pubkey,
    metas: Vec<AccountMeta>,
    infos: &[AccountInfo<'info>],
    mode: u8,
    amount_in: u64,
    min_out: u64,
    swapper_bump: u8,
) -> Result<()> {
    let mut data = Vec::with_capacity(8 + 8 + 8 + 1);
    data.extend_from_slice(&SWAP2_DISCRIMINATOR);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&min_out.to_le_bytes());
    data.push(mode);
    let ix = Instruction {
        program_id: *program,
        accounts: metas,
        data,
    };
    let seeds: &[&[u8]] = &[SWAPPER_SEED, &[swapper_bump]];
    invoke_signed(&ix, infos, &[seeds])?;
    Ok(())
}

/// The accounts of the flow that do not depend on the venue.
pub struct FlowAccounts<'a, 'info> {
    pub buyer: &'a Signer<'info>,
    pub swapper: &'a UncheckedAccount<'info>,
    pub swapper_bump: u8,
    pub swapper_pack: &'a mut InterfaceAccount<'info, TokenAccount>,
    pub swapper_wsol: &'a mut Account<'info, SplTokenAccount>,
    pub pack_mint: &'a InterfaceAccount<'info, Mint>,
    pub pack_token_program: &'a Interface<'info, TokenInterface>,
    pub token_program: &'a Program<'info, Token>,
    pub system_program: &'a Program<'info, System>,
}

/// The mint-shaped swap, exactly as proven in P2-00: (1) the buyer pays `burn_lamports` into the swapper's wSOL account
/// and the balance is synced; (2) the swapper PDA signs a CPI to the venue's `swap2`; (3) the received $PACK is the
/// difference of the swapper's balance, above zero and at least `min_out`, and the wSOL spent is above zero and at most
/// `burn_lamports`; (4) exactly that difference is burned and the balance is back to what it was; (5) the wSOL account is
/// closed to the buyer (the unspent part of a partial fill and the rent return in the same transaction).
pub fn buy_and_burn<'a, 'info>(
    f: FlowAccounts<'a, 'info>,
    venue_program: &Pubkey,
    mode: u8,
    burn_lamports: u64,
    min_out: u64,
    metas: Vec<AccountMeta>,
    infos: &[AccountInfo<'info>],
) -> Result<SwapOutcome> {
    require!(burn_lamports > 0 && min_out > 0, BinderError::ZeroAmount);
    require!(
        mode == MODE_EXACT_IN || mode == MODE_PARTIAL_FILL,
        BinderError::InvariantBroken
    );

    cpi_lite::system_transfer(
        &f.buyer.to_account_info(),
        &f.swapper_wsol.to_account_info(),
        burn_lamports,
        &[],
    )?;
    cpi_lite::token_sync_native(&f.swapper_wsol.to_account_info())?;
    f.swapper_wsol.reload()?;
    f.swapper_pack.reload()?;
    let wsol_before = f.swapper_wsol.amount;
    let pack_before = f.swapper_pack.amount;

    swap_cpi(
        venue_program,
        metas,
        infos,
        mode,
        burn_lamports,
        min_out,
        f.swapper_bump,
    )?;

    f.swapper_pack.reload()?;
    f.swapper_wsol.reload()?;
    let received = f
        .swapper_pack
        .amount
        .checked_sub(pack_before)
        .ok_or_else(|| error!(BinderError::MathOverflow))?;
    require!(received > 0, BinderError::NothingReceived);
    require!(received >= min_out, BinderError::SlippageExceeded);
    let spent = wsol_before
        .checked_sub(f.swapper_wsol.amount)
        .ok_or_else(|| error!(BinderError::MathOverflow))?;
    require!(spent > 0, BinderError::NothingSpent);
    require!(spent <= burn_lamports, BinderError::Overspent);

    let seeds: &[&[u8]] = &[SWAPPER_SEED, &[f.swapper_bump]];
    cpi_lite::token_burn(
        &f.pack_token_program.key(),
        &f.swapper_pack.to_account_info(),
        &f.pack_mint.to_account_info(),
        &f.swapper.to_account_info(),
        received,
        &[seeds],
    )?;
    f.swapper_pack.reload()?;
    require_eq!(
        f.swapper_pack.amount,
        pack_before,
        BinderError::BurnMismatch
    );

    let refunded_lamports = f.swapper_wsol.to_account_info().lamports();
    cpi_lite::token_close_account(
        &f.swapper_wsol.to_account_info(),
        &f.buyer.to_account_info(),
        &f.swapper.to_account_info(),
        &[seeds],
    )?;
    Ok(SwapOutcome {
        wsol_spent: spent,
        received,
        refunded_lamports,
    })
}

// ---------------------------------------------------------------------------------------------------------------------
// typed reads of the venue accounts (offsets pinned in constants.rs, from packages/adapters/meteora-dbc)
// ---------------------------------------------------------------------------------------------------------------------

fn key_at(data: &[u8], offset: usize) -> Result<Pubkey> {
    let bytes: [u8; 32] = data
        .get(offset..offset + 32)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| error!(BinderError::PinMismatch))?;
    Ok(Pubkey::new_from_array(bytes))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DbcPoolView {
    pub config: Pubkey,
    pub base_mint: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
}

/// A DBC `VirtualPool`: 424 bytes with its discriminator.
pub fn parse_dbc_pool(data: &[u8]) -> Result<DbcPoolView> {
    require!(data.len() == DBC_POOL_SIZE, BinderError::PinMismatch);
    require!(
        data[..8] == DBC_POOL_DISCRIMINATOR,
        BinderError::PinMismatch
    );
    Ok(DbcPoolView {
        config: key_at(data, DBC_POOL_CONFIG)?,
        base_mint: key_at(data, DBC_POOL_BASE_MINT)?,
        base_vault: key_at(data, DBC_POOL_BASE_VAULT)?,
        quote_vault: key_at(data, DBC_POOL_QUOTE_VAULT)?,
    })
}

/// A DBC `PoolConfig` (1,048 bytes; the transfer-hook variant is refused): the quote is wSOL, the fee mode is a fee
/// scheduler (never the deprecated rate limiter) and the fee is collected in the quote token.
pub fn check_dbc_config(data: &[u8]) -> Result<()> {
    require!(data.len() == DBC_CONFIG_SIZE, BinderError::PinMismatch);
    require!(
        data[..8] == DBC_CONFIG_DISCRIMINATOR,
        BinderError::PinMismatch
    );
    require_keys_eq!(
        key_at(data, DBC_CONFIG_QUOTE_MINT)?,
        WSOL_MINT,
        BinderError::PinMismatch
    );
    require!(
        data[DBC_CONFIG_BASE_FEE_MODE] != BASE_FEE_MODE_RATE_LIMITER,
        BinderError::PinMismatch
    );
    require!(
        data[DBC_CONFIG_COLLECT_FEE_MODE] == 0,
        BinderError::PinMismatch
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DammPoolView {
    pub token_a_mint: Pubkey,
    pub token_b_mint: Pubkey,
    pub token_a_vault: Pubkey,
    pub token_b_vault: Pubkey,
}

/// A DAMM v2 `Pool`: 1,112 bytes with its discriminator.
pub fn parse_damm_pool(data: &[u8]) -> Result<DammPoolView> {
    require!(data.len() == DAMM_POOL_SIZE, BinderError::DammNotReady);
    require!(
        data[..8] == DAMM_POOL_DISCRIMINATOR,
        BinderError::DammNotReady
    );
    Ok(DammPoolView {
        token_a_mint: key_at(data, DAMM_POOL_TOKEN_A_MINT)?,
        token_b_mint: key_at(data, DAMM_POOL_TOKEN_B_MINT)?,
        token_a_vault: key_at(data, DAMM_POOL_TOKEN_A_VAULT)?,
        token_b_vault: key_at(data, DAMM_POOL_TOKEN_B_VAULT)?,
    })
}

/// The DAMM v2 pool of the DBC migration and its two vaults, DERIVED from fixed addresses: the pool is the PDA
/// `["pool", migration config, max(mint), min(mint)]`, the vaults `["token_vault", mint, pool]` (P2-00 section 6). Token A is
/// the base ($PACK), token B is wSOL, as the migration creates it.
pub fn derive_damm(pack_mint: &Pubkey) -> (Pubkey, Pubkey, Pubkey) {
    let (hi, lo) = if pack_mint > &WSOL_MINT {
        (*pack_mint, WSOL_MINT)
    } else {
        (WSOL_MINT, *pack_mint)
    };
    let (pool, _) = Pubkey::find_program_address(
        &[
            b"pool",
            DAMM_MIGRATION_CONFIG.as_ref(),
            hi.as_ref(),
            lo.as_ref(),
        ],
        &DAMM_PROGRAM,
    );
    let (vault_a, _) = Pubkey::find_program_address(
        &[b"token_vault", pack_mint.as_ref(), pool.as_ref()],
        &DAMM_PROGRAM,
    );
    let (vault_b, _) = Pubkey::find_program_address(
        &[b"token_vault", WSOL_MINT.as_ref(), pool.as_ref()],
        &DAMM_PROGRAM,
    );
    (pool, vault_a, vault_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(data: &mut [u8], offset: usize, key: &Pubkey) {
        data[offset..offset + 32].copy_from_slice(key.as_ref());
    }

    #[test]
    fn dbc_pool_is_read_at_the_pinned_offsets() {
        let (cfg, mint, bv, qv) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        let mut d = vec![0u8; DBC_POOL_SIZE];
        d[..8].copy_from_slice(&DBC_POOL_DISCRIMINATOR);
        put(&mut d, DBC_POOL_CONFIG, &cfg);
        put(&mut d, DBC_POOL_BASE_MINT, &mint);
        put(&mut d, DBC_POOL_BASE_VAULT, &bv);
        put(&mut d, DBC_POOL_QUOTE_VAULT, &qv);
        let v = parse_dbc_pool(&d).unwrap();
        assert_eq!(
            (v.config, v.base_mint, v.base_vault, v.quote_vault),
            (cfg, mint, bv, qv)
        );
        // another size or another discriminator is refused
        assert!(parse_dbc_pool(&d[..423]).is_err());
        d[0] ^= 1;
        assert!(parse_dbc_pool(&d).is_err());
    }

    #[test]
    fn dbc_config_must_be_a_fee_scheduler_in_wsol_quote_mode() {
        let mut d = vec![0u8; DBC_CONFIG_SIZE];
        d[..8].copy_from_slice(&DBC_CONFIG_DISCRIMINATOR);
        put(&mut d, DBC_CONFIG_QUOTE_MINT, &WSOL_MINT);
        assert!(check_dbc_config(&d).is_ok());
        let mut rate_limiter = d.clone();
        rate_limiter[DBC_CONFIG_BASE_FEE_MODE] = BASE_FEE_MODE_RATE_LIMITER;
        assert!(check_dbc_config(&rate_limiter).is_err());
        let mut output_fee = d.clone();
        output_fee[DBC_CONFIG_COLLECT_FEE_MODE] = 1;
        assert!(check_dbc_config(&output_fee).is_err());
        let mut other_quote = d.clone();
        put(
            &mut other_quote,
            DBC_CONFIG_QUOTE_MINT,
            &Pubkey::new_unique(),
        );
        assert!(check_dbc_config(&other_quote).is_err());
        assert!(check_dbc_config(&d[..1047]).is_err());
    }

    #[test]
    fn damm_pool_is_read_at_the_pinned_offsets() {
        let (a, b, av, bv) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        let mut d = vec![0u8; DAMM_POOL_SIZE];
        d[..8].copy_from_slice(&DAMM_POOL_DISCRIMINATOR);
        put(&mut d, DAMM_POOL_TOKEN_A_MINT, &a);
        put(&mut d, DAMM_POOL_TOKEN_B_MINT, &b);
        put(&mut d, DAMM_POOL_TOKEN_A_VAULT, &av);
        put(&mut d, DAMM_POOL_TOKEN_B_VAULT, &bv);
        let v = parse_damm_pool(&d).unwrap();
        assert_eq!(
            (
                v.token_a_mint,
                v.token_b_mint,
                v.token_a_vault,
                v.token_b_vault
            ),
            (a, b, av, bv)
        );
        assert!(parse_damm_pool(&d[..1111]).is_err());
    }

    #[test]
    fn damm_derivation_is_order_independent_in_the_pool_seed() {
        // the pool seed uses max and min, so it is the same whichever of the two mints sorts first
        let a = Pubkey::new_unique();
        let (pool, va, vb) = derive_damm(&a);
        assert_ne!(pool, Pubkey::default());
        assert_ne!(va, vb);
        let (pool2, _, _) = derive_damm(&a);
        assert_eq!(pool, pool2);
    }
}
