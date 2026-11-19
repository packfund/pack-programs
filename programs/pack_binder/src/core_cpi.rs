//! The Metaplex Core calls of `pack_binder`, written by hand against the `mpl-core` 0.12.1 wire format (P2-10b).
//!
//! The `mpl-core` crate stays a dev-dependency only: its unit tests below build every instruction with the crate's own
//! builders and assert that the accounts (key, signer, writable) and the data bytes here are IDENTICAL. The crate is not
//! linked into the program because its generated CPI code, with every plugin and adapter type it can serialize, was about
//! 60 KB of the program and these four calls use a tiny part of it (reports/P2-10, docs/validation/P2-10_BINDER_SIZE.md).
//! Core is only ever called at the constant program id [`CORE_ID`].

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;

use crate::constants::{
    AUTHORITY_SEED, COLLECTION_NAME, CORE_KEY_UNINITIALIZED, ROYALTY_BPS, ROYALTY_POT_PERCENT,
    ROYALTY_TREASURY_PERCENT,
};
use crate::errors::BinderError;

/// The Metaplex Core program (`mpl_core::ID`).
pub const CORE_ID: Pubkey = pubkey!("CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d");

// Core instruction discriminators (one byte) and enum tags, from mpl-core 0.12.1 `generated/`.
const IX_CREATE_V2: u8 = 20;
const IX_CREATE_COLLECTION_V2: u8 = 21;
const IX_TRANSFER_V1: u8 = 14;
const IX_UPDATE_COLLECTION_PLUGIN_V1: u8 = 7;
const PLUGIN_ROYALTIES: u8 = 0;
const RULE_SET_PROGRAM_ALLOW_LIST: u8 = 1;
const DATA_STATE_ACCOUNT_STATE: u8 = 0;
/// `Key::AssetV1` of an account of the Core program.
pub const CORE_KEY_ASSET_V1: u8 = 1;
const CORE_KEY_LAST: u8 = 6;

/// One royalty creator (design D1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Creator {
    pub address: Pubkey,
    pub percentage: u8,
}

/// The Royalties plugin of the collection (design D1): the allow list is the only rule set the program ever writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Royalties {
    pub basis_points: u16,
    pub creators: Vec<Creator>,
    /// `RuleSet::ProgramAllowList(..)`
    pub rule_set: Vec<Pubkey>,
}

/// The Royalties plugin of the collection (design D1): 500 bps, the pot vault 60 and the Treasury 40 (3% and 2% of a
/// sale), and the allow list. `basis_points` and `creators` come from constants and `Config`, never from an argument.
pub fn royalties(pot: Pubkey, treasury: Pubkey, rules: Vec<Pubkey>) -> Royalties {
    Royalties {
        basis_points: ROYALTY_BPS,
        creators: vec![
            Creator {
                address: pot,
                percentage: ROYALTY_POT_PERCENT,
            },
            Creator {
                address: treasury,
                percentage: ROYALTY_TREASURY_PERCENT,
            },
        ],
        rule_set: rules,
    }
}

// ---- borsh writers (little endian, u32 length prefixes) ----

/// A borsh length prefix: a u32. A length that does not fit is an error, never a silent truncation.
fn put_len(out: &mut Vec<u8>, n: usize) -> Result<()> {
    let n = u32::try_from(n).map_err(|_| error!(BinderError::MathOverflow))?;
    out.extend_from_slice(&n.to_le_bytes());
    Ok(())
}

fn put_str(out: &mut Vec<u8>, s: &str) -> Result<()> {
    put_len(out, s.len())?;
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

/// `Plugin::Royalties(Royalties { basis_points, creators, rule_set: ProgramAllowList(..) })`
fn put_royalties_plugin(out: &mut Vec<u8>, r: &Royalties) -> Result<()> {
    out.push(PLUGIN_ROYALTIES);
    out.extend_from_slice(&r.basis_points.to_le_bytes());
    put_len(out, r.creators.len())?;
    for c in &r.creators {
        out.extend_from_slice(c.address.as_ref());
        out.push(c.percentage);
    }
    out.push(RULE_SET_PROGRAM_ALLOW_LIST);
    put_len(out, r.rule_set.len())?;
    for k in &r.rule_set {
        out.extend_from_slice(k.as_ref());
    }
    Ok(())
}

// ---- the four instructions: pure builders (tested against the crate) ----

/// A Core instruction before it is sent: the metas, the data, and which of them are in the account list of the CPI.
pub struct Built {
    pub metas: Vec<AccountMeta>,
    pub data: Vec<u8>,
}

/// `CreateCollectionV2`: data `[21] name uri Some([PluginAuthorityPair { Royalties, None }]) None`.
pub fn create_collection_ix(
    collection: Pubkey,
    update_authority: Pubkey,
    payer: Pubkey,
    system_program: Pubkey,
    uri: &str,
    royalties: &Royalties,
) -> Result<Built> {
    let mut data = vec![IX_CREATE_COLLECTION_V2];
    put_str(&mut data, COLLECTION_NAME)?;
    put_str(&mut data, uri)?;
    data.push(1); // plugins: Some
    put_len(&mut data, 1)?; // one PluginAuthorityPair
    put_royalties_plugin(&mut data, royalties)?;
    data.push(0); // PluginAuthorityPair.authority: None
    data.push(0); // external_plugin_adapters: None
    Ok(Built {
        metas: vec![
            AccountMeta::new(collection, true),
            AccountMeta::new_readonly(update_authority, false),
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(system_program, false),
        ],
        data,
    })
}

/// `UpdateCollectionPluginV1`: data `[7] Plugin::Royalties(..)`. The `log_wrapper` slot is Core's own address (None).
pub fn update_collection_royalties_ix(
    collection: Pubkey,
    payer: Pubkey,
    authority: Pubkey,
    system_program: Pubkey,
    royalties: &Royalties,
) -> Result<Built> {
    let mut data = vec![IX_UPDATE_COLLECTION_PLUGIN_V1];
    put_royalties_plugin(&mut data, royalties)?;
    Ok(Built {
        metas: vec![
            AccountMeta::new(collection, false),
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new_readonly(system_program, false),
            AccountMeta::new_readonly(CORE_ID, false),
        ],
        data,
    })
}

/// `CreateV2`: data `[20] DataState::AccountState name uri None None`. `update_authority` and `log_wrapper` are Core's own
/// address (None: the asset's update authority is then its collection).
#[allow(clippy::too_many_arguments)]
pub fn create_asset_ix(
    asset: Pubkey,
    collection: Pubkey,
    authority: Pubkey,
    payer: Pubkey,
    owner: Pubkey,
    system_program: Pubkey,
    name: &str,
    uri: &str,
) -> Result<Built> {
    let mut data = vec![IX_CREATE_V2, DATA_STATE_ACCOUNT_STATE];
    put_str(&mut data, name)?;
    put_str(&mut data, uri)?;
    data.push(0); // plugins: None
    data.push(0); // external_plugin_adapters: None
    Ok(Built {
        metas: vec![
            AccountMeta::new(asset, true),
            AccountMeta::new(collection, false),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(owner, false),
            AccountMeta::new_readonly(CORE_ID, false),
            AccountMeta::new_readonly(system_program, false),
            AccountMeta::new_readonly(CORE_ID, false),
        ],
        data,
    })
}

/// `TransferV1`: data `[14] None` (no compression proof). A missing collection and the `log_wrapper` slot are Core's own
/// address.
pub fn transfer_ix(
    asset: Pubkey,
    collection: Option<Pubkey>,
    payer: Pubkey,
    authority: Pubkey,
    new_owner: Pubkey,
    system_program: Pubkey,
) -> Built {
    Built {
        metas: vec![
            AccountMeta::new(asset, false),
            AccountMeta::new_readonly(collection.unwrap_or(CORE_ID), false),
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new_readonly(new_owner, false),
            AccountMeta::new_readonly(system_program, false),
            AccountMeta::new_readonly(CORE_ID, false),
        ],
        data: vec![IX_TRANSFER_V1, 0],
    }
}

/// Sends a built Core instruction. `infos` are the accounts of the metas that are not Core's own address, in any order;
/// the Core program account itself is always passed.
fn send<'info>(
    core_program: &AccountInfo<'info>,
    built: Built,
    infos: &[&AccountInfo<'info>],
    seeds: &[&[&[u8]]],
) -> Result<()> {
    let ix = Instruction {
        program_id: CORE_ID,
        accounts: built.metas,
        data: built.data,
    };
    let mut all: Vec<AccountInfo<'info>> = Vec::with_capacity(infos.len() + 1);
    all.push(core_program.clone());
    for i in infos {
        all.push((*i).clone());
    }
    invoke_signed(&ix, &all, seeds)?;
    Ok(())
}

/// `CreateCollectionV2`: the collection (a new keypair that signed the transaction), update authority the program's
/// `["authority"]` PDA, one plugin: the Royalties plugin with the update authority as its authority.
pub fn create_collection<'info>(
    core_program: &AccountInfo<'info>,
    collection: &AccountInfo<'info>,
    update_authority: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    uri: String,
    royalties: Royalties,
) -> Result<()> {
    let built = create_collection_ix(
        *collection.key,
        *update_authority.key,
        *payer.key,
        *system_program.key,
        &uri,
        &royalties,
    )?;
    send(
        core_program,
        built,
        &[collection, update_authority, payer, system_program],
        &[],
    )
}

/// `UpdateCollectionPluginV1` with the rebuilt Royalties plugin; the `["authority"]` PDA signs.
pub fn update_collection_royalties<'info>(
    core_program: &AccountInfo<'info>,
    collection: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    royalties: Royalties,
    authority_bump: u8,
) -> Result<()> {
    let seeds: &[&[u8]] = &[AUTHORITY_SEED, &[authority_bump]];
    let built = update_collection_royalties_ix(
        *collection.key,
        *payer.key,
        *authority.key,
        *system_program.key,
        &royalties,
    )?;
    send(
        core_program,
        built,
        &[collection, payer, authority, system_program],
        &[seeds],
    )
}

/// `CreateV2`: one Binder asset in the collection, owned by `owner`, created and signed for by the `["authority"]` PDA
/// (the collection's update authority). The asset is a new keypair that signed the transaction. The name and the URI are
/// built by the program.
#[allow(clippy::too_many_arguments)]
pub fn create_asset<'info>(
    core_program: &AccountInfo<'info>,
    asset: &AccountInfo<'info>,
    collection: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    owner: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    name: String,
    uri: String,
    authority_bump: u8,
) -> Result<()> {
    let seeds: &[&[u8]] = &[AUTHORITY_SEED, &[authority_bump]];
    let built = create_asset_ix(
        *asset.key,
        *collection.key,
        *authority.key,
        *payer.key,
        *owner.key,
        *system_program.key,
        &name,
        &uri,
    )?;
    send(
        core_program,
        built,
        &[asset, collection, authority, payer, owner, system_program],
        &[seeds],
    )
}

/// `TransferV1` of a prize Core asset owned by the vault PDA to a new owner; the vault PDA signs.
#[allow(clippy::too_many_arguments)]
pub fn transfer_prize<'info>(
    core_program: &AccountInfo<'info>,
    prize: &AccountInfo<'info>,
    prize_collection: Option<&AccountInfo<'info>>,
    payer: &AccountInfo<'info>,
    vault: &AccountInfo<'info>,
    new_owner: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    vault_seeds: &[&[u8]],
) -> Result<()> {
    let built = transfer_ix(
        *prize.key,
        prize_collection.map(|c| *c.key),
        *payer.key,
        *vault.key,
        *new_owner.key,
        *system_program.key,
    );
    let mut infos: Vec<&AccountInfo<'info>> = vec![prize, payer, vault, new_owner, system_program];
    if let Some(c) = prize_collection {
        infos.push(c);
    }
    send(core_program, built, &infos, &[vault_seeds])
}

/// Whether `info` is what Core leaves after `BurnV1`: an account owned by the Core program whose data is exactly one byte, the
/// `Key::Uninitialized` marker. A live asset, an account of another program, a missing account (Core never closes one to zero
/// lamports: the tombstone keeps its rent) and anything else is NOT burned. The tombstone keeps its address taken, so it cannot
/// come back as an asset.
pub fn is_burned_asset(info: &AccountInfo) -> Result<bool> {
    if *info.owner != CORE_ID || info.data_len() != 1 {
        return Ok(false);
    }
    let data = info.try_borrow_data()?;
    Ok(data.first() == Some(&CORE_KEY_UNINITIALIZED))
}

// ---- reading a Core asset (the head of `BaseAssetV1`) ----

/// The `update_authority` of a Core asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateAuth {
    None,
    Address(Pubkey),
    Collection(Pubkey),
}

/// The head of a Core `BaseAssetV1`: `key`, `owner`, `update_authority`; `name`, `uri` and `seq` are validated and dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssetHead {
    pub key: u8,
    pub owner: Pubkey,
    pub update_authority: UpdateAuth,
}

fn take<'a>(buf: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    if buf.len() < n {
        return None;
    }
    let (head, rest) = buf.split_at(n);
    *buf = rest;
    Some(head)
}

fn take_pubkey(buf: &mut &[u8]) -> Option<Pubkey> {
    Pubkey::try_from(take(buf, 32)?).ok()
}

/// A borsh `String`: a u32 length, then that many bytes of valid UTF-8.
fn skip_string(buf: &mut &[u8]) -> Option<()> {
    let n = take(buf, 4)?;
    let len = usize::try_from(u32::from_le_bytes([n[0], n[1], n[2], n[3]])).ok()?;
    core::str::from_utf8(take(buf, len)?).ok()?;
    Some(())
}

/// Parses `BaseAssetV1` the way `BaseAssetV1::from_bytes` (borsh) accepts it, and returns its head. `None` where the borsh
/// parse fails: a key above `GroupV1`, a short or malformed field, a bad enum or option tag, a name or uri that is not UTF-8.
/// Trailing bytes (the plugin header and registry) are ignored, as `from_bytes` does.
pub fn parse_base_asset(data: &[u8]) -> Option<AssetHead> {
    let mut buf = data;
    let key = take(&mut buf, 1)?[0];
    if key > CORE_KEY_LAST {
        return None;
    }
    let owner = take_pubkey(&mut buf)?;
    let update_authority = match take(&mut buf, 1)?[0] {
        0 => UpdateAuth::None,
        1 => UpdateAuth::Address(take_pubkey(&mut buf)?),
        2 => UpdateAuth::Collection(take_pubkey(&mut buf)?),
        _ => return None,
    };
    skip_string(&mut buf)?; // name
    skip_string(&mut buf)?; // uri
    match take(&mut buf, 1)?[0] {
        0 => {}
        1 => {
            take(&mut buf, 8)?; // seq
        }
        _ => return None,
    }
    Some(AssetHead {
        key,
        owner,
        update_authority,
    })
}

/// What a claim needs of a Binder asset.
pub struct BinderAsset {
    pub owner: Pubkey,
}

/// Reads a Binder asset: owned by the Core program, an `AssetV1`, whose update authority is the pinned collection.
/// Returns its current owner (Core keeps it up to date on every transfer).
pub fn read_binder_asset(info: &AccountInfo, collection: &Pubkey) -> Result<BinderAsset> {
    require_keys_eq!(*info.owner, CORE_ID, BinderError::WrongCoreProgram);
    let data = info.try_borrow_data()?;
    let asset = parse_base_asset(&data).ok_or_else(|| error!(BinderError::WrongCollection))?;
    require!(asset.key == CORE_KEY_ASSET_V1, BinderError::WrongCollection);
    match asset.update_authority {
        UpdateAuth::Collection(c) => {
            require_keys_eq!(c, *collection, BinderError::WrongCollection)
        }
        _ => return Err(error!(BinderError::WrongCollection)),
    }
    Ok(BinderAsset { owner: asset.owner })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::prelude::borsh;
    use mpl_core::accounts::BaseAssetV1;
    use mpl_core::instructions::{
        CreateCollectionV2Builder, CreateV2Builder, TransferV1Builder,
        UpdateCollectionPluginV1Builder,
    };
    use mpl_core::types::{
        Creator as CoreCreator, DataState, Key, Plugin, PluginAuthorityPair,
        Royalties as CoreRoyalties, RuleSet, UpdateAuthority,
    };

    fn burned(owner: &Pubkey, data: &mut [u8]) -> bool {
        let key = Pubkey::new_unique();
        let mut lamports = 2_155_320u64;
        let info = AccountInfo::new(&key, false, false, &mut lamports, data, owner, false);
        is_burned_asset(&info).unwrap()
    }

    #[test]
    fn the_core_program_id_is_the_crates() {
        assert_eq!(CORE_ID, mpl_core::ID);
    }

    #[test]
    fn only_the_one_byte_core_tombstone_is_a_burned_asset() {
        // what Core leaves after BurnV1 (mpl-core 0.12.1 and the cloned binary): owned by Core, one byte, Key::Uninitialized
        assert!(burned(&CORE_ID, &mut [CORE_KEY_UNINITIALIZED]));
        // not one byte (a live asset is far longer; a byte pair is not the tombstone)
        assert!(!burned(&CORE_ID, &mut [0, 0]));
        assert!(!burned(&CORE_ID, &mut []));
        // another owner: a program that is not Core can hold any bytes
        assert!(!burned(&system_program::ID, &mut [CORE_KEY_UNINITIALIZED]));
        assert!(!burned(
            &Pubkey::new_unique(),
            &mut [CORE_KEY_UNINITIALIZED]
        ));
        // another marker byte: AssetV1 (1) and every other key is not the tombstone
        for key in 1..=6u8 {
            assert!(!burned(&CORE_ID, &mut [key]), "key byte {key}");
        }
    }

    #[test]
    fn the_plugin_is_the_decision_d1_one() {
        let pot = Pubkey::new_unique();
        let treasury = Pubkey::new_unique();
        let system = Pubkey::default();
        let r = royalties(pot, treasury, vec![system]);
        assert_eq!(r.basis_points, 500);
        assert_eq!(r.creators.len(), 2);
        // the percentages sum to exactly 100 (Core refuses anything else) and the addresses are distinct
        let sum: u16 = r.creators.iter().map(|c| c.percentage as u16).sum();
        assert_eq!(sum, 100);
        assert_ne!(r.creators[0].address, r.creators[1].address);
        // 60 / 40 of 5% is 3% and 2%
        assert_eq!(
            r.basis_points as u32 * r.creators[0].percentage as u32 / 100,
            300
        );
        assert_eq!(
            r.basis_points as u32 * r.creators[1].percentage as u32 / 100,
            200
        );
        assert_eq!(r.rule_set, vec![system]);
    }

    // ---- the wire format against the mpl-core crate's own builders ----

    fn core_royalties(r: &Royalties) -> CoreRoyalties {
        CoreRoyalties {
            basis_points: r.basis_points,
            creators: r
                .creators
                .iter()
                .map(|c| CoreCreator {
                    address: c.address,
                    percentage: c.percentage,
                })
                .collect(),
            rule_set: RuleSet::ProgramAllowList(r.rule_set.clone()),
        }
    }

    fn sample_royalties(rules: usize) -> Royalties {
        royalties(
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            (0..rules).map(|_| Pubkey::new_unique()).collect(),
        )
    }

    #[test]
    fn create_collection_is_the_crates_instruction() {
        for rules in [0usize, 1, 5, 13] {
            let r = sample_royalties(rules);
            let (collection, ua, payer, sys) = (
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                Pubkey::new_unique(),
            );
            let uri = "https://example.invalid/collection.json";
            let mine = create_collection_ix(collection, ua, payer, sys, uri, &r).unwrap();
            let crate_ix = CreateCollectionV2Builder::new()
                .collection(collection)
                .update_authority(Some(ua))
                .payer(payer)
                .system_program(sys)
                .name(COLLECTION_NAME.to_string())
                .uri(uri.to_string())
                .plugins(vec![PluginAuthorityPair {
                    plugin: Plugin::Royalties(core_royalties(&r)),
                    authority: None,
                }])
                .instruction();
            assert_eq!(crate_ix.program_id, CORE_ID);
            assert_eq!(crate_ix.accounts, mine.metas, "accounts, {rules} rules");
            assert_eq!(crate_ix.data, mine.data, "data, {rules} rules");
        }
    }

    #[test]
    fn update_collection_royalties_is_the_crates_instruction() {
        for rules in [0usize, 1, 5, 13] {
            let r = sample_royalties(rules);
            let (collection, payer, authority, sys) = (
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                Pubkey::new_unique(),
            );
            let mine =
                update_collection_royalties_ix(collection, payer, authority, sys, &r).unwrap();
            let crate_ix = UpdateCollectionPluginV1Builder::new()
                .collection(collection)
                .payer(payer)
                .authority(Some(authority))
                .system_program(sys)
                .plugin(Plugin::Royalties(core_royalties(&r)))
                .instruction();
            assert_eq!(crate_ix.accounts, mine.metas, "accounts, {rules} rules");
            assert_eq!(crate_ix.data, mine.data, "data, {rules} rules");
        }
    }

    #[test]
    fn create_asset_is_the_crates_instruction() {
        let (asset, collection, authority, payer, owner, sys) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        // the real name and URI shapes, an empty string, and a long one with multi-byte characters
        for (name, uri) in [
            ("Pack Binder #1", "https://arweave.net/abc/1.json"),
            ("", ""),
            ("B\u{e9}\u{4e2d}\u{1f600} #999", &"u".repeat(200)),
        ] {
            let mine = create_asset_ix(asset, collection, authority, payer, owner, sys, name, uri)
                .unwrap();
            let crate_ix = CreateV2Builder::new()
                .asset(asset)
                .collection(Some(collection))
                .authority(Some(authority))
                .payer(payer)
                .owner(Some(owner))
                .update_authority(None)
                .system_program(sys)
                .data_state(DataState::AccountState)
                .name(name.to_string())
                .uri(uri.to_string())
                .instruction();
            assert_eq!(crate_ix.accounts, mine.metas);
            assert_eq!(crate_ix.data, mine.data);
        }
    }

    #[test]
    fn transfer_is_the_crates_instruction() {
        let (asset, collection, payer, authority, new_owner, sys) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        for col in [Some(collection), None] {
            let mine = transfer_ix(asset, col, payer, authority, new_owner, sys);
            let crate_ix = TransferV1Builder::new()
                .asset(asset)
                .collection(col)
                .payer(payer)
                .authority(Some(authority))
                .new_owner(new_owner)
                .system_program(Some(sys))
                .instruction();
            assert_eq!(crate_ix.accounts, mine.metas, "collection {col:?}");
            assert_eq!(crate_ix.data, mine.data, "collection {col:?}");
        }
    }

    // ---- reading an asset against BaseAssetV1::from_bytes ----

    fn asset_bytes(
        key: Key,
        ua: UpdateAuthority,
        name: &str,
        uri: &str,
        seq: Option<u64>,
    ) -> (BaseAssetV1, Vec<u8>) {
        let a = BaseAssetV1 {
            key,
            owner: Pubkey::new_unique(),
            update_authority: ua,
            name: name.to_string(),
            uri: uri.to_string(),
            seq,
        };
        let bytes = borsh::to_vec(&a).unwrap();
        (a, bytes)
    }

    fn same_as_crate(data: &[u8]) {
        let theirs = BaseAssetV1::from_bytes(data);
        let mine = parse_base_asset(data);
        assert_eq!(
            theirs.is_ok(),
            mine.is_some(),
            "accepted differently: {} bytes {:02x?}",
            data.len(),
            &data[..data.len().min(80)]
        );
        if let (Ok(t), Some(m)) = (theirs, mine) {
            assert_eq!(t.key as u8, m.key);
            assert_eq!(t.owner, m.owner);
            let ua = match t.update_authority {
                UpdateAuthority::None => UpdateAuth::None,
                UpdateAuthority::Address(k) => UpdateAuth::Address(k),
                UpdateAuthority::Collection(k) => UpdateAuth::Collection(k),
            };
            assert_eq!(ua, m.update_authority);
        }
    }

    #[test]
    fn the_asset_head_parses_as_the_crate_does() {
        let col = Pubkey::new_unique();
        let uas = [
            UpdateAuthority::None,
            UpdateAuthority::Address(Pubkey::new_unique()),
            UpdateAuthority::Collection(col),
        ];
        let keys = [
            Key::Uninitialized,
            Key::AssetV1,
            Key::HashedAssetV1,
            Key::PluginHeaderV1,
            Key::PluginRegistryV1,
            Key::CollectionV1,
            Key::GroupV1,
        ];
        let mut n = 0;
        for key in keys {
            for ua in &uas {
                for seq in [None, Some(7u64)] {
                    for (name, uri) in [
                        ("Pack Binder #1", "https://x/1.json"),
                        ("", ""),
                        ("\u{e9}", "\u{4e2d}"),
                    ] {
                        let (a, mut bytes) = asset_bytes(key, ua.clone(), name, uri, seq);
                        same_as_crate(&bytes);
                        let m = parse_base_asset(&bytes).unwrap();
                        assert_eq!(m.owner, a.owner);
                        // trailing bytes (the plugin header and registry of a live asset) are ignored
                        bytes.extend_from_slice(&[9, 8, 7, 6, 5, 4, 3, 2, 1]);
                        same_as_crate(&bytes);
                        // every truncation, every length: both refuse the same ones
                        for cut in 0..bytes.len() {
                            same_as_crate(&bytes[..cut]);
                        }
                        n += 1;
                    }
                }
            }
        }
        assert_eq!(n, 7 * 3 * 2 * 3);
    }

    #[test]
    fn the_asset_head_refuses_what_the_crate_refuses() {
        let (_, base) = asset_bytes(
            Key::AssetV1,
            UpdateAuthority::Collection(Pubkey::new_unique()),
            "Pack Binder #1",
            "https://x/1.json",
            Some(3),
        );
        // every single byte set to every value, and every pair of bytes at a few positions: same verdict as the crate
        for pos in 0..base.len() {
            for v in [0u8, 1, 2, 3, 6, 7, 0x7f, 0x80, 0xc0, 0xfe, 0xff] {
                let mut b = base.clone();
                b[pos] = v;
                same_as_crate(&b);
            }
        }
        // a deterministic pseudo random sweep of whole buffers and of mutated real assets
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..4000 {
            let mut b = base.clone();
            for _ in 0..(next() % 4 + 1) {
                let p = (next() as usize) % b.len();
                b[p] = next() as u8;
            }
            let cut = (next() as usize) % (b.len() + 8);
            b.truncate(cut.min(b.len()));
            same_as_crate(&b);
        }
        for _ in 0..2000 {
            let len = (next() as usize) % 200;
            let b: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            same_as_crate(&b);
        }
        // invalid UTF-8 in the name or the uri
        for bad in [[0xffu8, 0xfe], [0xc3, 0x28]] {
            let mut b = Vec::new();
            b.push(1u8);
            b.extend_from_slice(Pubkey::new_unique().as_ref());
            b.push(2);
            b.extend_from_slice(Pubkey::new_unique().as_ref());
            b.extend_from_slice(&2u32.to_le_bytes());
            b.extend_from_slice(&bad);
            b.extend_from_slice(&0u32.to_le_bytes());
            b.push(0);
            same_as_crate(&b);
            assert!(parse_base_asset(&b).is_none());
        }
    }
    // ---- read_binder_asset on hand-built accounts (independent review, finding F2) ----

    /// The name of the Anchor error `read_binder_asset` answers, or `Ok` with the owner it returns.
    fn read(
        owner: &Pubkey,
        data: &mut [u8],
        collection: &Pubkey,
    ) -> std::result::Result<Pubkey, String> {
        let key = Pubkey::new_unique();
        let mut lamports = 2_155_320u64;
        let info = AccountInfo::new(&key, false, false, &mut lamports, data, owner, false);
        match read_binder_asset(&info, collection) {
            Ok(a) => Ok(a.owner),
            Err(Error::AnchorError(e)) => Err(e.error_name),
            Err(_) => Err("not an Anchor error".to_string()),
        }
    }

    fn ua_collection(c: Pubkey) -> UpdateAuthority {
        UpdateAuthority::Collection(c)
    }

    #[test]
    fn read_binder_asset_accepts_only_a_core_asset_of_the_pinned_collection() {
        let col = Pubkey::new_unique();
        let (a, bytes) = asset_bytes(
            Key::AssetV1,
            ua_collection(col),
            "Pack Binder #7",
            "https://x/7.json",
            None,
        );
        // the control: a Core-owned AssetV1 whose update authority is the collection returns its owner
        assert_eq!(read(&CORE_ID, &mut bytes.clone(), &col), Ok(a.owner));
        // with a sequence number and trailing bytes (the plugin header and registry of a live asset) too
        let (a2, mut with_seq) = asset_bytes(
            Key::AssetV1,
            ua_collection(col),
            "Pack Binder #7",
            "https://x/7.json",
            Some(9),
        );
        with_seq.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(read(&CORE_ID, &mut with_seq, &col), Ok(a2.owner));

        // an account of another program is WrongCoreProgram, whatever its bytes say
        for owner in [
            system_program::ID,
            Pubkey::new_unique(),
            anchor_spl::token::ID,
        ] {
            assert_eq!(
                read(&owner, &mut bytes.clone(), &col),
                Err("WrongCoreProgram".to_string()),
                "owner {owner}"
            );
        }

        // every Core key but AssetV1 (0 Uninitialized, 2 HashedAssetV1 .. 6 GroupV1) is WrongCollection, even with the right collection
        for key in [
            Key::Uninitialized,
            Key::HashedAssetV1,
            Key::PluginHeaderV1,
            Key::PluginRegistryV1,
            Key::CollectionV1,
            Key::GroupV1,
        ] {
            let (_, b) = asset_bytes(
                key,
                ua_collection(col),
                "Pack Binder #7",
                "https://x/7.json",
                None,
            );
            assert_eq!(
                read(&CORE_ID, &mut b.clone(), &col),
                Err("WrongCollection".to_string()),
                "key {}",
                b[0]
            );
        }

        // another collection, an Address update authority (even the collection's own address) and None are WrongCollection
        let other = Pubkey::new_unique();
        for ua in [
            ua_collection(other),
            UpdateAuthority::Address(col),
            UpdateAuthority::Address(other),
            UpdateAuthority::None,
        ] {
            let (_, b) = asset_bytes(Key::AssetV1, ua, "Pack Binder #7", "https://x/7.json", None);
            assert_eq!(
                read(&CORE_ID, &mut b.clone(), &col),
                Err("WrongCollection".to_string())
            );
        }

        // bytes that do not parse (empty, truncated at every length, a bad UTF-8 name) are WrongCollection, never a panic
        assert_eq!(
            read(&CORE_ID, &mut [], &col),
            Err("WrongCollection".to_string())
        );
        for cut in 0..bytes.len() {
            assert_eq!(
                read(&CORE_ID, &mut bytes[..cut].to_vec(), &col),
                Err("WrongCollection".to_string()),
                "cut {cut}"
            );
        }
        let mut bad_utf8 = bytes.clone();
        let name_at = 1 + 32 + 1 + 32 + 4; // key, owner, update authority tag and key, the name's length prefix
        bad_utf8[name_at] = 0xff;
        assert_eq!(
            read(&CORE_ID, &mut bad_utf8, &col),
            Err("WrongCollection".to_string())
        );
    }
}
