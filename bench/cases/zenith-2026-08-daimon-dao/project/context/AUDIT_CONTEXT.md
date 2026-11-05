# Audit context: Daimon DAO

The repository README at the audited commit. In scope:

- `src/DaimonGovernor.sol`
- `src/DaimonMigration.sol`
- `src/DaimonStaking.sol`
- `src/DaimonTimelock.sol`
- `src/DaimonV2.sol`

---

<div align="center">

<img src="social-assets/logo-512.png" alt="Daimon DAO" width="140" />

# Daimon DAO

**No owner. No mint. Floor 21B. — DAO on BNB Chain**

</div>

---

Daimon è un token BEP-20 con reflection, staking vote-escrow, governance
on-chain e timelock pubblico, su BNB Chain / PancakeSwap. Nessun owner,
nessuna funzione di mint, floor di supply immutabile a 21 miliardi: tutto è
verificabile on-chain, il deployer rinuncia a ogni ruolo dopo il deploy.

## Proprietà chiave

- **Nessun owner, nessun mint.** Il controllo è della DAO via Timelock; il
  deployer non trattiene alcun ruolo (verificato on-chain e dagli invariant).
- **Floor immutabile 21B.** La supply può solo scendere (burn deflazionario)
  e mai sotto `MIN_SUPPLY`, enforced a livello di codice.
- **Timelock pubblico di 7 giorni** su ogni azione di governance — finestra
  di reazione per la community, valida anche per la DAO stessa.
- **Vote-escrow.** Il potere di voto deriva solo da token bloccati nel tempo,
  fotografato allo snapshot della proposta (niente flash-loan governance).

## Contratti (`src/`)

| Contratto | Ruolo |
|---|---|
| `DaimonV2` | Token BEP-20: reflection, fee autonome, buyback&burn, floor 21B (UUPS) |
| `DaimonStaking` | Staking vote-escrow, voting power con checkpoint, reward in BNB |
| `DaimonGovernor` | Governance: propose → voto → queue → execute, quorum su snapshot |
| `DaimonTimelock` | Timelock hardcodato a 7 giorni su ogni esecuzione |
| `DaimonMigration` | Migrazione 1:1 dal vecchio token, sweep post-deadline alla treasury |

## Stato

Contratti deployati e verificati su **BSC testnet**; suite di test (unit +
fuzz + invariant + avversariali, **74 test verdi**) e analisi statica
Slither eseguite. **Non ancora sottoposti ad audit professionale esterno** —
il deploy mainnet avverrà solo dopo l'audit.

## Documentazione

- [THREAT_MODEL.md](THREAT_MODEL.md) — modello di minaccia, attori, difese,
  limiti noti e scelte di design
- [SECURITY.md](SECURITY.md) — come segnalare vulnerabilità (responsible
  disclosure)
- [TESTNET_RESULTS.md](TESTNET_RESULTS.md) — risultati dei test end-to-end su
  testnet reale
- [DEPLOY.md](DEPLOY.md) — procedura di deploy
- [daimon-dapp/](daimon-dapp/) — dApp ufficiale (Next.js + wagmi), con la sua
  [README](daimon-dapp/README.md)

## Sicurezza

Hai trovato una vulnerabilità? **Non aprire una issue pubblica.** Usa il
canale privato (GitHub → Security → Report a vulnerability) — dettagli in
[SECURITY.md](SECURITY.md).

## Build & test

```sh
forge build
forge test
```

Il progetto richiede `via_ir = true` (la matematica reflection genera "stack
too deep" senza), EVM `shanghai` per BSC. Vedi `foundry.toml`.
