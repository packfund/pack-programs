//! The system-program and SPL token calls of the program, written by hand against the instruction encodings of the
//! system program, Token and Token-2022 (P2-10c).
//!
//! `anchor_lang::system_program` and `anchor_spl::{token, token_interface}` build the same instructions through the
//! `solana-system-interface` and `spl-token` crates, which link about 30 KB of builders, a `bincode` serializer and a
//! `TokenInstruction` packer into the program. The few calls the program makes are fixed layouts, so they are written
//! here, and the unit tests below build every one of them with the crates' own builders and assert that the program id,
//! the accounts (key, signer, writable, in order) and the data bytes are IDENTICAL.
//!
//! Where Anchor's wrapper ignores the program id it is given and always calls the classic Token program
//! (`token::close_account`, `token::sync_native`), so do these (`token_close_account`, `token_sync_native`).
//!
//! The crates' own builders (`spl_token_2022::instruction::*`) refuse a program id that is not Token or Token-2022
//! (`IncorrectProgramId`); the three builders here that take a program id do the same (`check_token_program`), so a
//! token call can never be aimed at another program, whatever a caller passes (independent review, finding F3).

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;

/// The classic Token program (`spl_token::ID`).
pub const CLASSIC_TOKEN_ID: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
/// The Token-2022 program (`spl_token_2022::ID`).
pub const TOKEN_2022_ID: Pubkey = pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

// System program instruction tags (u32, little endian, as bincode writes the enum) and Token instruction tags (one byte).
const SYS_CREATE_ACCOUNT: u32 = 0;
const SYS_ASSIGN: u32 = 1;
const SYS_TRANSFER: u32 = 2;
const SYS_ALLOCATE: u32 = 8;
const TOKEN_BURN: u8 = 8;
const TOKEN_CLOSE_ACCOUNT: u8 = 9;
const TOKEN_TRANSFER_CHECKED: u8 = 12;
const TOKEN_SYNC_NATIVE: u8 = 17;
const TOKEN_INITIALIZE_ACCOUNT3: u8 = 18;

/// Token or Token-2022, nothing else: the check of the crates' own builders (`IncorrectProgramId`).
fn check_token_program(token_program: &Pubkey) -> Result<()> {
    if *token_program != CLASSIC_TOKEN_ID && *token_program != TOKEN_2022_ID {
        return Err(ProgramError::IncorrectProgramId.into());
    }
    Ok(())
}

/// An instruction before it is sent.
pub struct Built {
    pub program_id: Pubkey,
    pub metas: Vec<AccountMeta>,
    pub data: Vec<u8>,
}

fn sys_data(tag: u32, size: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(size);
    data.extend_from_slice(&tag.to_le_bytes());
    data
}

/// `SystemInstruction::Transfer { lamports }`
pub fn system_transfer_ix(from: Pubkey, to: Pubkey, lamports: u64) -> Built {
    let mut data = sys_data(SYS_TRANSFER, 12);
    data.extend_from_slice(&lamports.to_le_bytes());
    Built {
        program_id: system_program::ID,
        metas: vec![AccountMeta::new(from, true), AccountMeta::new(to, false)],
        data,
    }
}

/// `SystemInstruction::CreateAccount { lamports, space, owner }`
pub fn system_create_account_ix(
    from: Pubkey,
    to: Pubkey,
    lamports: u64,
    space: u64,
    owner: Pubkey,
) -> Built {
    let mut data = sys_data(SYS_CREATE_ACCOUNT, 52);
    data.extend_from_slice(&lamports.to_le_bytes());
    data.extend_from_slice(&space.to_le_bytes());
    data.extend_from_slice(owner.as_ref());
    Built {
        program_id: system_program::ID,
        metas: vec![AccountMeta::new(from, true), AccountMeta::new(to, true)],
        data,
    }
}

/// `SystemInstruction::Allocate { space }`
pub fn system_allocate_ix(account: Pubkey, space: u64) -> Built {
    let mut data = sys_data(SYS_ALLOCATE, 12);
    data.extend_from_slice(&space.to_le_bytes());
    Built {
        program_id: system_program::ID,
        metas: vec![AccountMeta::new(account, true)],
        data,
    }
}

/// `SystemInstruction::Assign { owner }`
pub fn system_assign_ix(account: Pubkey, owner: Pubkey) -> Built {
    let mut data = sys_data(SYS_ASSIGN, 36);
    data.extend_from_slice(owner.as_ref());
    Built {
        program_id: system_program::ID,
        metas: vec![AccountMeta::new(account, true)],
        data,
    }
}

/// `TokenInstruction::TransferChecked { amount, decimals }`, no multisig.
pub fn token_transfer_checked_ix(
    token_program: Pubkey,
    from: Pubkey,
    mint: Pubkey,
    to: Pubkey,
    authority: Pubkey,
    amount: u64,
    decimals: u8,
) -> Result<Built> {
    check_token_program(&token_program)?;
    let mut data = Vec::with_capacity(10);
    data.push(TOKEN_TRANSFER_CHECKED);
    data.extend_from_slice(&amount.to_le_bytes());
    data.push(decimals);
    Ok(Built {
        program_id: token_program,
        metas: vec![
            AccountMeta::new(from, false),
            AccountMeta::new_readonly(mint, false),
            AccountMeta::new(to, false),
            AccountMeta::new_readonly(authority, true),
        ],
        data,
    })
}

/// `TokenInstruction::Burn { amount }`, no multisig.
pub fn token_burn_ix(
    token_program: Pubkey,
    account: Pubkey,
    mint: Pubkey,
    authority: Pubkey,
    amount: u64,
) -> Result<Built> {
    check_token_program(&token_program)?;
    let mut data = Vec::with_capacity(9);
    data.push(TOKEN_BURN);
    data.extend_from_slice(&amount.to_le_bytes());
    Ok(Built {
        program_id: token_program,
        metas: vec![
            AccountMeta::new(account, false),
            AccountMeta::new(mint, false),
            AccountMeta::new_readonly(authority, true),
        ],
        data,
    })
}

/// `TokenInstruction::CloseAccount`, no multisig, always the classic Token program.
pub fn token_close_account_ix(account: Pubkey, destination: Pubkey, authority: Pubkey) -> Built {
    Built {
        program_id: CLASSIC_TOKEN_ID,
        metas: vec![
            AccountMeta::new(account, false),
            AccountMeta::new(destination, false),
            AccountMeta::new_readonly(authority, true),
        ],
        data: vec![TOKEN_CLOSE_ACCOUNT],
    }
}

/// `TokenInstruction::SyncNative`, always the classic Token program.
pub fn token_sync_native_ix(account: Pubkey) -> Built {
    Built {
        program_id: CLASSIC_TOKEN_ID,
        metas: vec![AccountMeta::new(account, false)],
        data: vec![TOKEN_SYNC_NATIVE],
    }
}

/// `TokenInstruction::InitializeAccount3 { owner }`
pub fn token_initialize_account3_ix(
    token_program: Pubkey,
    account: Pubkey,
    mint: Pubkey,
    owner: Pubkey,
) -> Result<Built> {
    check_token_program(&token_program)?;
    let mut data = Vec::with_capacity(33);
    data.push(TOKEN_INITIALIZE_ACCOUNT3);
    data.extend_from_slice(owner.as_ref());
    Ok(Built {
        program_id: token_program,
        metas: vec![
            AccountMeta::new(account, false),
            AccountMeta::new_readonly(mint, false),
        ],
        data,
    })
}

/// Sends a built instruction. `infos` are the accounts of its metas, in any order; the program account is not needed
/// (the runtime finds the callee among the accounts of the outer instruction, as for Anchor's own CPI wrappers).
fn send<'info>(built: Built, infos: &[&AccountInfo<'info>], seeds: &[&[&[u8]]]) -> Result<()> {
    let ix = Instruction {
        program_id: built.program_id,
        accounts: built.metas,
        data: built.data,
    };
    let all: Vec<AccountInfo<'info>> = infos.iter().map(|i| (*i).clone()).collect();
    invoke_signed(&ix, &all, seeds)?;
    Ok(())
}

pub fn system_transfer<'info>(
    from: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    lamports: u64,
    seeds: &[&[&[u8]]],
) -> Result<()> {
    send(
        system_transfer_ix(*from.key, *to.key, lamports),
        &[from, to],
        seeds,
    )
}

pub fn system_create_account<'info>(
    from: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    lamports: u64,
    space: u64,
    owner: &Pubkey,
    seeds: &[&[&[u8]]],
) -> Result<()> {
    send(
        system_create_account_ix(*from.key, *to.key, lamports, space, *owner),
        &[from, to],
        seeds,
    )
}

pub fn system_allocate<'info>(
    account: &AccountInfo<'info>,
    space: u64,
    seeds: &[&[&[u8]]],
) -> Result<()> {
    send(system_allocate_ix(*account.key, space), &[account], seeds)
}

pub fn system_assign<'info>(
    account: &AccountInfo<'info>,
    owner: &Pubkey,
    seeds: &[&[&[u8]]],
) -> Result<()> {
    send(system_assign_ix(*account.key, *owner), &[account], seeds)
}

#[allow(clippy::too_many_arguments)]
pub fn token_transfer_checked<'info>(
    token_program: &Pubkey,
    from: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    amount: u64,
    decimals: u8,
    seeds: &[&[&[u8]]],
) -> Result<()> {
    send(
        token_transfer_checked_ix(
            *token_program,
            *from.key,
            *mint.key,
            *to.key,
            *authority.key,
            amount,
            decimals,
        )?,
        &[from, mint, to, authority],
        seeds,
    )
}

pub fn token_burn<'info>(
    token_program: &Pubkey,
    account: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    amount: u64,
    seeds: &[&[&[u8]]],
) -> Result<()> {
    send(
        token_burn_ix(
            *token_program,
            *account.key,
            *mint.key,
            *authority.key,
            amount,
        )?,
        &[account, mint, authority],
        seeds,
    )
}

pub fn token_close_account<'info>(
    account: &AccountInfo<'info>,
    destination: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    seeds: &[&[&[u8]]],
) -> Result<()> {
    send(
        token_close_account_ix(*account.key, *destination.key, *authority.key),
        &[account, destination, authority],
        seeds,
    )
}

pub fn token_sync_native<'info>(account: &AccountInfo<'info>) -> Result<()> {
    send(token_sync_native_ix(*account.key), &[account], &[])
}

pub fn token_initialize_account3<'info>(
    token_program: &Pubkey,
    account: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    owner: &Pubkey,
) -> Result<()> {
    send(
        token_initialize_account3_ix(*token_program, *account.key, *mint.key, *owner)?,
        &[account, mint],
        &[],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::system_instruction;
    use anchor_spl::token::spl_token;
    use anchor_spl::token_2022::spl_token_2022;

    fn same(theirs: Instruction, mine: Built, what: &str) {
        assert_eq!(theirs.program_id, mine.program_id, "{what}: program id");
        assert_eq!(theirs.accounts, mine.metas, "{what}: accounts");
        assert_eq!(theirs.data, mine.data, "{what}: data");
    }

    fn keys<const N: usize>() -> [Pubkey; N] {
        core::array::from_fn(|_| Pubkey::new_unique())
    }

    const AMOUNTS: [u64; 6] = [0, 1, 255, 65_536, 1_000_000_000, u64::MAX];

    #[test]
    fn the_token_ids_are_the_crates() {
        assert_eq!(CLASSIC_TOKEN_ID, spl_token::ID);
        assert_eq!(TOKEN_2022_ID, spl_token_2022::ID);
    }

    fn refused(r: Result<Built>) -> bool {
        matches!(r, Err(Error::ProgramError(e)) if e.program_error == ProgramError::IncorrectProgramId)
    }

    #[test]
    fn the_token_builders_refuse_a_program_that_is_not_a_token_program() {
        let [from, mint, to, authority, owner] = keys();
        // the system program, the associated token program, the default key and strangers: the crates' own builders refuse them
        // too (`IncorrectProgramId`), and so do ours
        for tp in [
            system_program::ID,
            anchor_spl::associated_token::ID,
            Pubkey::default(),
            Pubkey::new_unique(),
        ] {
            assert!(
                spl_token_2022::instruction::burn(&tp, &from, &mint, &authority, &[], 1).is_err()
            );
            assert!(
                refused(token_transfer_checked_ix(
                    tp, from, mint, to, authority, 1, 6
                )),
                "transfer_checked {tp}"
            );
            assert!(
                refused(token_burn_ix(tp, from, mint, authority, 1)),
                "burn {tp}"
            );
            assert!(
                refused(token_initialize_account3_ix(tp, from, mint, owner)),
                "initialize_account3 {tp}"
            );
        }
        // and Token and Token-2022 pass
        for tp in [spl_token::ID, spl_token_2022::ID] {
            assert!(token_transfer_checked_ix(tp, from, mint, to, authority, 1, 6).is_ok());
            assert!(token_burn_ix(tp, from, mint, authority, 1).is_ok());
            assert!(token_initialize_account3_ix(tp, from, mint, owner).is_ok());
        }
    }

    #[test]
    fn system_calls_are_the_crates_instructions() {
        let [a, b, owner] = keys();
        for n in AMOUNTS {
            same(
                system_instruction::transfer(&a, &b, n),
                system_transfer_ix(a, b, n),
                "transfer",
            );
            same(
                system_instruction::allocate(&a, n),
                system_allocate_ix(a, n),
                "allocate",
            );
            for space in AMOUNTS {
                same(
                    system_instruction::create_account(&a, &b, n, space, &owner),
                    system_create_account_ix(a, b, n, space, owner),
                    "create_account",
                );
            }
        }
        same(
            system_instruction::assign(&a, &owner),
            system_assign_ix(a, owner),
            "assign",
        );
    }

    #[test]
    fn token_calls_are_the_crates_instructions() {
        let [from, mint, to, authority, owner] = keys();
        for tp in [spl_token::ID, spl_token_2022::ID] {
            for n in AMOUNTS {
                for decimals in [0u8, 6, 9, 255] {
                    same(
                        spl_token_2022::instruction::transfer_checked(
                            &tp,
                            &from,
                            &mint,
                            &to,
                            &authority,
                            &[],
                            n,
                            decimals,
                        )
                        .unwrap(),
                        token_transfer_checked_ix(tp, from, mint, to, authority, n, decimals)
                            .unwrap(),
                        "transfer_checked",
                    );
                }
                same(
                    spl_token_2022::instruction::burn(&tp, &from, &mint, &authority, &[], n)
                        .unwrap(),
                    token_burn_ix(tp, from, mint, authority, n).unwrap(),
                    "burn",
                );
            }
            same(
                spl_token_2022::instruction::initialize_account3(&tp, &from, &mint, &owner)
                    .unwrap(),
                token_initialize_account3_ix(tp, from, mint, owner).unwrap(),
                "initialize_account3",
            );
        }
        // the classic-only calls: Anchor's `token::close_account` and `token::sync_native` use `spl_token::ID`
        same(
            spl_token::instruction::close_account(&spl_token::ID, &from, &to, &authority, &[])
                .unwrap(),
            token_close_account_ix(from, to, authority),
            "close_account",
        );
        same(
            spl_token::instruction::sync_native(&spl_token::ID, &from).unwrap(),
            token_sync_native_ix(from),
            "sync_native",
        );
        // Token-2022 encodes these two the same way
        let t22 = spl_token_2022::instruction::close_account(
            &spl_token_2022::ID,
            &from,
            &to,
            &authority,
            &[],
        )
        .unwrap();
        assert_eq!(t22.data, token_close_account_ix(from, to, authority).data);
        assert_eq!(
            t22.accounts,
            token_close_account_ix(from, to, authority).metas
        );
        let t22 = spl_token_2022::instruction::sync_native(&spl_token_2022::ID, &from).unwrap();
        assert_eq!(t22.data, token_sync_native_ix(from).data);
        assert_eq!(t22.accounts, token_sync_native_ix(from).metas);
    }
}
