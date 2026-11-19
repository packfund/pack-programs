//! The $PACK mint check, as `pack_bond`: no freeze authority; if it is a Token-2022 mint, its extensions are only
//! MetadataPointer and TokenMetadata (what pump.fun creates; a transfer fee, a transfer hook or a permanent delegate could
//! block or drain the swapper's token account and break "every $PACK received is burned"). Extensions cannot be added to an
//! initialised mint, so the check at `init_config` time stays true. A classic SPL mint is accepted (the DBC launch of
//! $PACK, devnet and test mints).

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{spl_token_2022, Mint};
use spl_token_2022::extension::{BaseStateWithExtensions, ExtensionType, StateWithExtensions};

use crate::errors::BinderError;

pub fn validate_pack_mint(mint: &InterfaceAccount<Mint>) -> Result<()> {
    require!(mint.freeze_authority.is_none(), BinderError::MintNotPinned);
    require!(mint.is_initialized, BinderError::MintNotPinned);
    let info = mint.to_account_info();
    if *info.owner == spl_token_2022::ID {
        let data = info.try_borrow_data()?;
        let state = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&data)
            .map_err(|_| error!(BinderError::MintNotPinned))?;
        let extensions = state
            .get_extension_types()
            .map_err(|_| error!(BinderError::MintNotPinned))?;
        for extension in extensions {
            require!(
                matches!(
                    extension,
                    ExtensionType::MetadataPointer | ExtensionType::TokenMetadata
                ),
                BinderError::MintNotPinned
            );
        }
    }
    Ok(())
}
