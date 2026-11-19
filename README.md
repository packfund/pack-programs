# Pack Protocol on-chain programs

The Solana programs of Pack Protocol (https://pack.fund), published so that anyone can rebuild them and compare the result with the
programs deployed on chain.

| Program | Mainnet program id |
|---|---|
| `pack_binder` | `Ex8ffvBE3sTMyUtyytKRCrzbfuVs5ytsNLmvW2BZ8Sgi` |

## Verify

Requires [`solana-verify`](https://github.com/Ellipsis-Labs/solana-verifiable-build) and Docker.

```
solana-verify verify-from-repo https://github.com/packfund/pack-programs \
  --program-id Ex8ffvBE3sTMyUtyytKRCrzbfuVs5ytsNLmvW2BZ8Sgi \
  --library-name pack_binder \
  --commit-hash <commit> \
  --url https://api.mainnet-beta.solana.com
```

The build uses Agave 4.3.0 (`[workspace.metadata.cli]` in `Cargo.toml`) and produces SBPF v3.
