//! The two reads of the BPF Upgradeable Loader state that `init_config` and `init_collection` need, parsed by hand.
//!
//! anchor-lang's `ProgramData` and `Program::programdata_address` deserialize `UpgradeableLoaderState` with `bincode`
//! and serde. That pulls in serde's `Unexpected` error text, whose `Display` formats a float, and with it the whole
//! `core::fmt` float machinery (about 30 KB of program). The state is four fixed shapes (bincode, little endian,
//! fixed-int), so reading the few bytes needed is exact and needs none of it:
//!
//! ```text
//! Uninitialized                                  u32 0
//! Buffer      { authority_address: Option<Pubkey> }   u32 1, u8 tag, [32]
//! Program     { programdata_address: Pubkey }         u32 2, [32]
//! ProgramData { slot: u64, upgrade_authority_address: Option<Pubkey> }  u32 3, u64, u8 tag, [32]
//! ```

use anchor_lang::prelude::*;
use anchor_lang::solana_program::bpf_loader_upgradeable;

const TAG_PROGRAM: u32 = 2;
const TAG_PROGRAM_DATA: u32 = 3;

fn bad() -> Error {
    ProgramError::InvalidAccountData.into()
}

fn take<'a>(buf: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
    if buf.len() < n {
        return Err(bad());
    }
    let (head, rest) = buf.split_at(n);
    *buf = rest;
    Ok(head)
}

fn take_u32(buf: &mut &[u8]) -> Result<u32> {
    let b = take(buf, 4)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn take_pubkey(buf: &mut &[u8]) -> Result<Pubkey> {
    let b = take(buf, 32)?;
    Pubkey::try_from(b).map_err(|_| bad())
}

/// `Program { programdata_address }` of a program account owned by the upgradeable loader, as
/// `Program::programdata_address`. `None` for a program account of another loader.
pub fn programdata_address(program: &AccountInfo) -> Result<Option<Pubkey>> {
    if *program.owner != bpf_loader_upgradeable::ID {
        return Ok(None);
    }
    let data = program.try_borrow_data()?;
    let mut buf: &[u8] = &data;
    if take_u32(&mut buf)? != TAG_PROGRAM {
        return Err(bad());
    }
    Ok(Some(take_pubkey(&mut buf)?))
}

/// The `ProgramData` account of a program: only the upgrade authority is read (the slot is parsed and dropped).
#[derive(Clone)]
pub struct ProgramDataAuthority {
    pub upgrade_authority_address: Option<Pubkey>,
}

impl AccountDeserialize for ProgramDataAuthority {
    fn try_deserialize(buf: &mut &[u8]) -> Result<Self> {
        Self::try_deserialize_unchecked(buf)
    }

    fn try_deserialize_unchecked(buf: &mut &[u8]) -> Result<Self> {
        match take_u32(buf)? {
            TAG_PROGRAM_DATA => {}
            0..=2 => return Err(anchor_lang::error::ErrorCode::AccountNotProgramData.into()),
            _ => return Err(bad()),
        }
        take(buf, 8)?; // the deploy height, not needed
        let upgrade_authority_address = match take(buf, 1)?[0] {
            0 => None,
            1 => Some(take_pubkey(buf)?),
            _ => return Err(bad()),
        };
        Ok(Self {
            upgrade_authority_address,
        })
    }
}

impl AccountSerialize for ProgramDataAuthority {
    fn try_serialize<W: std::io::Write>(&self, _writer: &mut W) -> Result<()> {
        Ok(())
    }
}

impl Owner for ProgramDataAuthority {
    fn owner() -> Pubkey {
        bpf_loader_upgradeable::ID
    }
}

#[cfg(feature = "idl-build")]
mod idl_build {
    use super::*;

    impl anchor_lang::IdlBuild for ProgramDataAuthority {}
    impl anchor_lang::Discriminator for ProgramDataAuthority {
        const DISCRIMINATOR: &'static [u8] = &[];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::bpf_loader_upgradeable::UpgradeableLoaderState;

    fn states() -> Vec<UpgradeableLoaderState> {
        let k = Pubkey::new_unique();
        vec![
            UpgradeableLoaderState::Uninitialized,
            UpgradeableLoaderState::Buffer {
                authority_address: None,
            },
            UpgradeableLoaderState::Buffer {
                authority_address: Some(k),
            },
            UpgradeableLoaderState::Program {
                programdata_address: k,
            },
            UpgradeableLoaderState::ProgramData {
                slot: 0,
                upgrade_authority_address: None,
            },
            UpgradeableLoaderState::ProgramData {
                slot: u64::MAX,
                upgrade_authority_address: Some(k),
            },
        ]
    }

    fn info_of<'a>(
        key: &'a Pubkey,
        owner: &'a Pubkey,
        lamports: &'a mut u64,
        data: &'a mut [u8],
    ) -> AccountInfo<'a> {
        AccountInfo::new(key, false, false, lamports, data, owner, true)
    }

    /// The hand parse and anchor-lang's bincode parse give the same verdict, and the same upgrade authority.
    fn same_program_data(data: &[u8]) {
        let theirs = ProgramData::try_deserialize_unchecked(&mut &data[..]);
        let mine = ProgramDataAuthority::try_deserialize_unchecked(&mut &data[..]);
        assert_eq!(
            theirs.is_ok(),
            mine.is_ok(),
            "accepted differently: {} bytes {:02x?}",
            data.len(),
            &data[..data.len().min(60)]
        );
        if let (Ok(t), Ok(m)) = (theirs, mine) {
            assert_eq!(t.upgrade_authority_address, m.upgrade_authority_address);
        }
    }

    fn same_programdata_address(data: &mut [u8]) {
        let (key, lamports_owner) = (Pubkey::new_unique(), bpf_loader_upgradeable::ID);
        let mut lamports = 1u64;
        let theirs = UpgradeableLoaderState::try_deserialize_unchecked(&mut &data[..]);
        let info = info_of(&key, &lamports_owner, &mut lamports, data);
        let mine = programdata_address(&info);
        match theirs {
            Ok(UpgradeableLoaderState::Program {
                programdata_address,
            }) => assert_eq!(mine.unwrap(), Some(programdata_address)),
            // anchor-lang panics (`unreachable!`) on another state of an executable account, which cannot happen on chain;
            // the hand parse refuses it
            _ => assert!(mine.is_err()),
        }
    }

    #[test]
    fn every_loader_state_parses_as_anchor_does() {
        for st in states() {
            let mut bytes = bincode::serialize(&st).unwrap();
            same_program_data(&bytes);
            same_programdata_address(&mut bytes.clone());
            // a ProgramData account carries the ELF after its 45 byte header; trailing bytes are ignored
            bytes.extend_from_slice(&[0x7f, b'E', b'L', b'F', 1, 2, 3]);
            same_program_data(&bytes);
            same_programdata_address(&mut bytes.clone());
            // every truncation
            for cut in 0..bytes.len() {
                same_program_data(&bytes[..cut]);
                same_programdata_address(&mut bytes[..cut].to_vec());
            }
        }
        // the value read is the upgrade authority itself
        let k = Pubkey::new_unique();
        let b = bincode::serialize(&UpgradeableLoaderState::ProgramData {
            slot: 5,
            upgrade_authority_address: Some(k),
        })
        .unwrap();
        let m = ProgramDataAuthority::try_deserialize_unchecked(&mut &b[..]).unwrap();
        assert_eq!(m.upgrade_authority_address, Some(k));
        // an authority-less (frozen) program reads as None, never as an authority
        let b = bincode::serialize(&UpgradeableLoaderState::ProgramData {
            slot: 5,
            upgrade_authority_address: None,
        })
        .unwrap();
        let m = ProgramDataAuthority::try_deserialize_unchecked(&mut &b[..]).unwrap();
        assert_eq!(m.upgrade_authority_address, None);
        // the program account of another loader has no program data address
        let (key, owner) = (Pubkey::new_unique(), Pubkey::new_unique());
        let mut lamports = 1u64;
        let mut data = bincode::serialize(&UpgradeableLoaderState::Program {
            programdata_address: k,
        })
        .unwrap();
        let info = info_of(&key, &owner, &mut lamports, &mut data);
        assert_eq!(programdata_address(&info).unwrap(), None);
    }

    #[test]
    fn garbage_is_judged_as_anchor_judges_it() {
        let base = bincode::serialize(&UpgradeableLoaderState::ProgramData {
            slot: 77,
            upgrade_authority_address: Some(Pubkey::new_unique()),
        })
        .unwrap();
        // every byte of the header set to a spread of values (enum tag, option tag, and the rest)
        for pos in 0..base.len() {
            for v in [0u8, 1, 2, 3, 4, 7, 0x80, 0xff] {
                let mut b = base.clone();
                b[pos] = v;
                same_program_data(&b);
                same_programdata_address(&mut b);
            }
        }
        let mut x: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..5000 {
            let len = (next() as usize) % 80;
            let len = if next() % 3 == 0 { 45 + len } else { len };
            let mut b: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            // bias the first bytes towards the real tags so the deeper branches run
            if b.len() >= 13 && next() % 2 == 0 {
                b[0] = (next() % 5) as u8;
                b[1] = 0;
                b[2] = 0;
                b[3] = 0;
                b[12] = (next() % 3) as u8;
            }
            same_program_data(&b);
            same_programdata_address(&mut b);
        }
    }
}
