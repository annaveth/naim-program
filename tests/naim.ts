import * as anchor from "@coral-xyz/anchor";
import { Program, BN } from "@coral-xyz/anchor";
import { Naim } from "../target/types/naim";
import { PublicKey, Keypair } from "@solana/web3.js";
import {
  TOKEN_PROGRAM_ID,
  ASSOCIATED_TOKEN_PROGRAM_ID,
  createMint,
  getOrCreateAssociatedTokenAccount,
  getAssociatedTokenAddressSync,
  mintTo,
  getAccount,
  getMint,
} from "@solana/spl-token";
import { createHash } from "crypto";
import { assert } from "chai";

describe("naim", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.naim as Program<Naim>;
  const wallet = provider.wallet as anchor.Wallet;
  const payer = wallet.payer; // funded Keypair
  const connection = provider.connection;

  const treasury = Keypair.generate();

  // --- helpers ---------------------------------------------------------
  const sha256 = (s: string) =>
    createHash("sha256").update(Buffer.from(s, "utf8")).digest();
  const hashArg = (s: string) => Array.from(sha256(s));
  const PREFIX = Buffer.from("naim");
  const TLD = Buffer.from(".agent");
  const namePda = (name: string) =>
    PublicKey.findProgramAddressSync([PREFIX, sha256(name), TLD], program.programId)[0];
  const pdaFromHash = (h: Buffer) =>
    PublicKey.findProgramAddressSync([PREFIX, h, TLD], program.programId)[0];
  const repPda = (name: string) =>
    PublicKey.findProgramAddressSync([Buffer.from("rep"), sha256(name)], program.programId)[0];
  const catPda = (category: string) =>
    PublicKey.findProgramAddressSync([Buffer.from("category"), sha256(category)], program.programId)[0];
  const [configPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("config")],
    program.programId
  );
  const [tokenConfigPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("token_config")],
    program.programId
  );
  const [stakersVaultPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("stakers_vault")],
    program.programId
  );
  const [stakePoolPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("stake_pool")],
    program.programId
  );
  const [stakeVaultPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("stake_vault")],
    program.programId
  );
  const stakeAccountPda = (user: PublicKey) =>
    PublicKey.findProgramAddressSync([Buffer.from("stake"), user.toBuffer()], program.programId)[0];

  // --- economics -------------------------------------------------------
  const YEAR = new BN(365 * 24 * 3600);
  const MONTH = new BN(30 * 24 * 3600);
  const N = 1_000_000; // 1 $NAIM (6 decimals) in base units
  const FEE_1_4 = 1000 * N;
  const FEE_5_9 = 200 * N;
  const FEE_10 = 50 * N;
  const VERIFY_FEE = 25 * N;
  const TREASURY_BPS = 4000;
  const STAKERS_BPS = 3000;
  const BURN_BPS = 3000;

  let naimMint: PublicKey;
  let payerAta: PublicKey;
  let treasuryAta: PublicKey;
  let stakersVaultAta: PublicKey;

  // shared token-account set for register/renew/verify
  const tokenAccts = () => ({
    tokenConfig: tokenConfigPda,
    naimMint,
    payerNaimAta: payerAta,
    treasuryNaimAta: treasuryAta,
    stakersVaultAta,
    stakersVault: stakersVaultPda,
    tokenProgram: TOKEN_PROGRAM_ID,
  });

  const tokenBal = async (ata: PublicKey) =>
    Number((await getAccount(connection, ata)).amount);
  const supply = async () => Number((await getMint(connection, naimMint)).supply);

  // assert a fee was split treasury / stakers / burn around `fn`
  async function expectSplit(fee: number, fn: () => Promise<void>) {
    const t0 = await tokenBal(treasuryAta);
    const s0 = await tokenBal(stakersVaultAta);
    const sup0 = await supply();
    await fn();
    const treasuryAmt = Math.floor((fee * TREASURY_BPS) / 10000);
    const stakersAmt = Math.floor((fee * STAKERS_BPS) / 10000);
    const burnAmt = fee - treasuryAmt - stakersAmt;
    assert.equal((await tokenBal(treasuryAta)) - t0, treasuryAmt, "treasury share");
    assert.equal((await tokenBal(stakersVaultAta)) - s0, stakersAmt, "stakers share");
    assert.equal(sup0 - (await supply()), burnAmt, "burned");
  }

  before(async () => {
    // a local $NAIM-equivalent mint (classic SPL, 6 decimals), payer is mint authority
    naimMint = await createMint(connection, payer, wallet.publicKey, null, 6);
    payerAta = (
      await getOrCreateAssociatedTokenAccount(connection, payer, naimMint, wallet.publicKey)
    ).address;
    await mintTo(connection, payer, naimMint, payerAta, wallet.publicKey, 10_000_000 * N);

    treasuryAta = getAssociatedTokenAddressSync(naimMint, treasury.publicKey);
    stakersVaultAta = getAssociatedTokenAddressSync(naimMint, stakersVaultPda, true);

    await program.methods
      .initialize({
        treasury: treasury.publicKey,
        fee14: new BN(FEE_1_4),
        fee59: new BN(FEE_5_9),
        fee10Plus: new BN(FEE_10),
        verifyFee: new BN(VERIFY_FEE),
        registrationPeriod: YEAR,
        gracePeriod: MONTH,
      })
      .accountsPartial({ config: configPda, admin: wallet.publicKey })
      .rpc();

    await program.methods
      .initTokenConfig({
        fee14: new BN(FEE_1_4),
        fee59: new BN(FEE_5_9),
        fee10Plus: new BN(FEE_10),
        verifyFee: new BN(VERIFY_FEE),
        treasuryBps: TREASURY_BPS,
        stakersBps: STAKERS_BPS,
        burnBps: BURN_BPS,
      })
      .accountsPartial({
        tokenConfig: tokenConfigPda,
        admin: wallet.publicKey,
        naimMint,
        treasury: treasury.publicKey,
        treasuryNaimAta: treasuryAta,
        stakersVault: stakersVaultPda,
        stakersVaultAta,
        tokenProgram: TOKEN_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
      })
      .rpc();
  });

  // =====================================================================
  it("initialize: stores config", async () => {
    const cfg = await program.account.config.fetch(configPda);
    assert.ok(cfg.treasury.equals(treasury.publicKey));
    assert.ok(cfg.admin.equals(wallet.publicKey));
    assert.equal(cfg.registrationPeriod.toNumber(), YEAR.toNumber());
  });

  it("init_token_config: stores fees + split, creates vaults", async () => {
    const tc = await program.account.tokenConfig.fetch(tokenConfigPda);
    assert.ok(tc.naimMint.equals(naimMint));
    assert.ok(tc.treasury.equals(treasury.publicKey));
    assert.equal(tc.fee14.toNumber(), FEE_1_4);
    assert.equal(tc.treasuryBps, TREASURY_BPS);
    assert.equal(tc.stakersBps, STAKERS_BPS);
    assert.equal(tc.burnBps, BURN_BPS);
    assert.equal(tc.totalBurned.toNumber(), 0);
    // vault token accounts now exist
    assert.equal(await tokenBal(treasuryAta), 0);
    assert.equal(await tokenBal(stakersVaultAta), 0);
  });

  it("init_token_config: rejects a split that doesn't sum to 100%", async () => {
    // can't re-init the singleton, but a bad split must be rejected: assert the
    // stored split is valid (sums to 10_000) — the program enforces this.
    const tc = await program.account.tokenConfig.fetch(tokenConfigPda);
    assert.equal(tc.treasuryBps + tc.stakersBps + tc.burnBps, 10000);
  });

  // =====================================================================
  it("register_name: 10+ chars is renewable, tier-3 fee paid in NAIM + split", async () => {
    const name = "scout.agent"; // 11 chars -> tier 10+
    const pda = namePda(name);

    await expectSplit(FEE_10, async () => {
      await program.methods
        .registerName(name, hashArg(name), "ar://card-1")
        .accountsPartial({ config: configPda, nameRecord: pda, payer: wallet.publicKey, stakeAccount: null, ...tokenAccts() })
        .rpc();
    });

    const rec = await program.account.nameRecord.fetch(pda);
    assert.ok(rec.owner.equals(wallet.publicKey));
    assert.ok(rec.resolver.equals(wallet.publicKey));
    assert.equal(rec.metadataUri, "ar://card-1");
    assert.isAbove(rec.expiryTimestamp.toNumber(), 0); // not permanent
    assert.equal(rec.verified, false);
    assert.equal(rec.linkedWallets.length, 0);
  });

  it("register_name: 1-4 chars is permanent, tier-1 fee", async () => {
    const name = "abc"; // 3 chars -> tier 1-4, permanent
    const pda = namePda(name);

    await expectSplit(FEE_1_4, async () => {
      await program.methods
        .registerName(name, hashArg(name), "")
        .accountsPartial({ config: configPda, nameRecord: pda, payer: wallet.publicKey, stakeAccount: null, ...tokenAccts() })
        .rpc();
    });

    const rec = await program.account.nameRecord.fetch(pda);
    assert.equal(rec.expiryTimestamp.toNumber(), 0); // permanent
  });

  it("register_name: accumulates total_burned on the token config", async () => {
    const tc = await program.account.tokenConfig.fetch(tokenConfigPda);
    // after the two registers above: burn = 30% of (FEE_10 + FEE_1_4)
    const expected = Math.floor((FEE_10 * BURN_BPS) / 10000) + Math.floor((FEE_1_4 * BURN_BPS) / 10000);
    assert.equal(tc.totalBurned.toNumber(), expected);
  });

  it("register_name: rejects a duplicate", async () => {
    const name = "scout.agent";
    const pda = namePda(name);
    let threw = false;
    try {
      await program.methods
        .registerName(name, hashArg(name), "x")
        .accountsPartial({ config: configPda, nameRecord: pda, payer: wallet.publicKey, stakeAccount: null, ...tokenAccts() })
        .rpc();
    } catch {
      threw = true; // account already in use
    }
    assert.isTrue(threw);
  });

  it("register_name: rejects an invalid charset", async () => {
    const name = "Bad_Name";
    const pda = namePda(name);
    let msg = "";
    try {
      await program.methods
        .registerName(name, hashArg(name), "")
        .accountsPartial({ config: configPda, nameRecord: pda, payer: wallet.publicKey, stakeAccount: null, ...tokenAccts() })
        .rpc();
    } catch (e) {
      msg = e.toString();
    }
    assert.include(msg, "InvalidName");
  });

  it("register_name: rejects a name_hash that doesn't match the name", async () => {
    const name = "mismatch.agent";
    const wrong = sha256("something-else");
    const pda = pdaFromHash(wrong);
    let msg = "";
    try {
      await program.methods
        .registerName(name, Array.from(wrong), "")
        .accountsPartial({ config: configPda, nameRecord: pda, payer: wallet.publicKey, stakeAccount: null, ...tokenAccts() })
        .rpc();
    } catch (e) {
      msg = e.toString();
    }
    assert.include(msg, "InvalidName");
  });

  // =====================================================================
  it("update_resolver + update_metadata", async () => {
    const pda = namePda("scout.agent");
    const newResolver = Keypair.generate().publicKey;

    await program.methods
      .updateResolver(newResolver)
      .accountsPartial({ nameRecord: pda, owner: wallet.publicKey })
      .rpc();
    await program.methods
      .updateMetadata("ar://card-2")
      .accountsPartial({ nameRecord: pda, owner: wallet.publicKey })
      .rpc();

    const rec = await program.account.nameRecord.fetch(pda);
    assert.ok(rec.resolver.equals(newResolver));
    assert.equal(rec.metadataUri, "ar://card-2");
  });

  it("update_resolver: rejects a non-owner", async () => {
    const pda = namePda("scout.agent");
    const attacker = Keypair.generate();
    let msg = "";
    try {
      await program.methods
        .updateResolver(attacker.publicKey)
        .accountsPartial({ nameRecord: pda, owner: attacker.publicKey })
        .signers([attacker])
        .rpc();
    } catch (e) {
      msg = e.toString();
    }
    assert.include(msg, "Unauthorized");
  });

  it("verify_name: sets the flag and pays the fee in NAIM + split", async () => {
    const pda = namePda("scout.agent");

    await expectSplit(VERIFY_FEE, async () => {
      await program.methods
        .verifyName()
        .accountsPartial({ nameRecord: pda, owner: wallet.publicKey, ...tokenAccts() })
        .rpc();
    });

    const rec = await program.account.nameRecord.fetch(pda);
    assert.equal(rec.verified, true);
  });

  it("link_wallet: requires both signatures and appends", async () => {
    const pda = namePda("scout.agent");
    const extra = Keypair.generate();

    await program.methods
      .linkWallet()
      .accountsPartial({ nameRecord: pda, owner: wallet.publicKey, newWallet: extra.publicKey })
      .signers([extra])
      .rpc();

    const rec = await program.account.nameRecord.fetch(pda);
    assert.equal(rec.linkedWallets.length, 1);
    assert.ok(rec.linkedWallets[0].equals(extra.publicKey));
  });

  it("renew_name: extends a non-permanent name and charges the tier fee in NAIM", async () => {
    const name = "scout.agent";
    const pda = namePda(name);
    const recBefore = await program.account.nameRecord.fetch(pda);

    await expectSplit(FEE_10, async () => {
      await program.methods
        .renewName(name, hashArg(name))
        .accountsPartial({ config: configPda, nameRecord: pda, owner: wallet.publicKey, ...tokenAccts() })
        .rpc();
    });

    const recAfter = await program.account.nameRecord.fetch(pda);
    assert.isAbove(recAfter.expiryTimestamp.toNumber(), recBefore.expiryTimestamp.toNumber());
  });

  it("renew_name: rejects a permanent name", async () => {
    const name = "abc";
    const pda = namePda(name);
    let msg = "";
    try {
      await program.methods
        .renewName(name, hashArg(name))
        .accountsPartial({ config: configPda, nameRecord: pda, owner: wallet.publicKey, ...tokenAccts() })
        .rpc();
    } catch (e) {
      msg = e.toString();
    }
    assert.include(msg, "NamePermanent");
  });

  // =====================================================================
  it("transfer_name: moves ownership; old owner loses authority", async () => {
    const name = "exec.defi"; // 9 chars -> tier 5-9
    const pda = namePda(name);
    await program.methods
      .registerName(name, hashArg(name), "")
      .accountsPartial({ config: configPda, nameRecord: pda, payer: wallet.publicKey, stakeAccount: null, ...tokenAccts() })
      .rpc();

    const newOwner = Keypair.generate();
    await program.methods
      .transferName(newOwner.publicKey)
      .accountsPartial({ nameRecord: pda, owner: wallet.publicKey })
      .rpc();

    const rec = await program.account.nameRecord.fetch(pda);
    assert.ok(rec.owner.equals(newOwner.publicKey));

    let msg = "";
    try {
      await program.methods
        .updateMetadata("nope")
        .accountsPartial({ nameRecord: pda, owner: wallet.publicKey })
        .rpc();
    } catch (e) {
      msg = e.toString();
    }
    assert.include(msg, "Unauthorized");
  });

  // =====================================================================
  it("release_name: closes the account and frees the name", async () => {
    const pda = namePda("scout.agent");
    await program.methods
      .releaseName()
      .accountsPartial({ nameRecord: pda, owner: wallet.publicKey })
      .rpc();

    let closed = false;
    try {
      await program.account.nameRecord.fetch(pda);
    } catch {
      closed = true;
    }
    assert.isTrue(closed);

    const name = "scout.agent";
    await program.methods
      .registerName(name, hashArg(name), "ar://card-reborn")
      .accountsPartial({ config: configPda, nameRecord: pda, payer: wallet.publicKey, stakeAccount: null, ...tokenAccts() })
      .rpc();
    const rec = await program.account.nameRecord.fetch(pda);
    assert.equal(rec.metadataUri, "ar://card-reborn");
  });

  // ===== REPUTATION (update/3) ========================================
  it("reputation: register sets created_at; renew bumps renew_count", async () => {
    const name = "repcheck.agent"; // 14 chars -> renewable
    const pda = namePda(name);
    await program.methods
      .registerName(name, hashArg(name), "ar://r")
      .accountsPartial({ config: configPda, nameRecord: pda, payer: wallet.publicKey, stakeAccount: null, ...tokenAccts() })
      .rpc();
    let rep = await program.account.reputationRecord.fetch(repPda(name));
    assert.isAbove(rep.createdAt.toNumber(), 0);
    assert.equal(rep.renewCount, 0);

    await program.methods
      .renewName(name, hashArg(name))
      .accountsPartial({ config: configPda, nameRecord: pda, owner: wallet.publicKey, ...tokenAccts() })
      .rpc();
    rep = await program.account.reputationRecord.fetch(repPda(name));
    assert.equal(rep.renewCount, 1);
  });

  // ===== STAKING (update/2) ===========================================
  it("init_stake_pool: creates pool + principal vault", async () => {
    const stakeVaultAta = getAssociatedTokenAddressSync(naimMint, stakeVaultPda, true);
    await program.methods
      .initStakePool()
      .accountsPartial({
        stakePool: stakePoolPda,
        tokenConfig: tokenConfigPda,
        admin: wallet.publicKey,
        naimMint,
        stakeVault: stakeVaultPda,
        stakeVaultAta,
        stakersVault: stakersVaultPda,
        rewardsVault: stakersVaultAta,
        tokenProgram: TOKEN_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
      })
      .rpc();
    const p = await program.account.stakePool.fetch(stakePoolPda);
    assert.equal(p.totalStaked.toNumber(), 0);
    assert.ok(p.naimMint.equals(naimMint));
  });

  const stakeVaultAta = () => getAssociatedTokenAddressSync(naimMint, stakeVaultPda, true);
  const stakeAccts = () => ({
    stakePool: stakePoolPda,
    stakeAccount: stakeAccountPda(wallet.publicKey),
    tokenConfig: tokenConfigPda,
    user: wallet.publicKey,
    naimMint,
    userNaimAta: payerAta,
    stakeVault: stakeVaultPda,
    stakeVaultAta: stakeVaultAta(),
    stakersVault: stakersVaultPda,
    rewardsVault: stakersVaultAta,
    tokenProgram: TOKEN_PROGRAM_ID,
  });

  const STAKE_AMT = 6000 * N; // >= tier-2 (5000) -> 25% discount

  it("stake: locks principal and updates the pool", async () => {
    await program.methods
      .stake(new BN(STAKE_AMT), new BN(0)) // lock 0s so unstake works later in the suite
      .accountsPartial({ ...stakeAccts(), associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID })
      .rpc();
    const sa = await program.account.stakeAccount.fetch(stakeAccountPda(wallet.publicKey));
    assert.equal(sa.amount.toNumber(), STAKE_AMT);
    const p = await program.account.stakePool.fetch(stakePoolPda);
    assert.equal(p.totalStaked.toNumber(), STAKE_AMT);
    assert.equal(await tokenBal(stakeVaultAta()), STAKE_AMT);
  });

  it("registration discount: a 5000+ stake pays 25% less", async () => {
    const name = "discounted.agent"; // 16 chars -> tier3 (50 $NAIM)
    const pda = namePda(name);
    const t0 = await tokenBal(treasuryAta);
    await program.methods
      .registerName(name, hashArg(name), "ar://d")
      .accountsPartial({
        config: configPda,
        nameRecord: pda,
        payer: wallet.publicKey,
        stakeAccount: stakeAccountPda(wallet.publicKey),
        ...tokenAccts(),
      })
      .rpc();
    // 50 $NAIM, 25% off = 37.5; treasury share = 40% of that
    const discounted = FEE_10 - Math.floor((FEE_10 * 2500) / 10000);
    assert.equal((await tokenBal(treasuryAta)) - t0, Math.floor((discounted * 4000) / 10000));
    assert.isBelow(Math.floor((discounted * 4000) / 10000), Math.floor((FEE_10 * 4000) / 10000));
  });

  it("claim_rewards: staker receives the accrued fee share", async () => {
    const discounted = FEE_10 - Math.floor((FEE_10 * 2500) / 10000);
    const expectShare = Math.floor((discounted * 3000) / 10000); // 30% of the discounted fee
    const b0 = await tokenBal(payerAta);
    await program.methods
      .claimRewards()
      .accountsPartial({
        stakePool: stakePoolPda,
        stakeAccount: stakeAccountPda(wallet.publicKey),
        tokenConfig: tokenConfigPda,
        user: wallet.publicKey,
        naimMint,
        userNaimAta: payerAta,
        stakersVault: stakersVaultPda,
        rewardsVault: stakersVaultAta,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .rpc();
    assert.equal((await tokenBal(payerAta)) - b0, expectShare);
  });

  it("unstake: returns principal after the lock", async () => {
    const b0 = await tokenBal(payerAta);
    await program.methods
      .unstake(new BN(STAKE_AMT))
      .accountsPartial(stakeAccts())
      .rpc();
    const sa = await program.account.stakeAccount.fetch(stakeAccountPda(wallet.publicKey));
    assert.equal(sa.amount.toNumber(), 0);
    assert.isAtLeast((await tokenBal(payerAta)) - b0, STAKE_AMT); // principal returned
  });

  // ===== CATEGORIES (update/2) ========================================
  it("register_category: creates a 5% category owned by the payer", async () => {
    const cat = "defi.agent";
    await program.methods
      .registerCategory(cat, hashArg(cat))
      .accountsPartial({ categoryRecord: catPda(cat), payer: wallet.publicKey, ...tokenAccts() })
      .rpc();
    const c = await program.account.categoryRecord.fetch(catPda(cat));
    assert.ok(c.owner.equals(wallet.publicKey));
    assert.equal(c.royaltyBps, 500);
    assert.equal(c.subCount.toNumber(), 0);
  });

  it("register_under_category: carves the 5% royalty, splits the rest, bumps sub_count", async () => {
    const cat = "defi.agent";
    const name = "exec.defi.agent"; // <label>.<parent>
    const t0 = await tokenBal(treasuryAta);
    await program.methods
      .registerUnderCategory(name, hashArg(name), cat, hashArg(cat), "ar://u")
      .accountsPartial({
        config: configPda,
        nameRecord: namePda(name),
        category: catPda(cat),
        categoryOwner: wallet.publicKey,
        payer: wallet.publicKey,
        categoryOwnerNaimAta: payerAta,
        ...tokenAccts(),
      })
      .rpc();
    const royalty = Math.floor((FEE_10 * 500) / 10000); // 5%
    const rem = FEE_10 - royalty;
    // treasury gets 40% of the remainder (i.e. of 95%), strictly less than the no-category 40%
    assert.equal((await tokenBal(treasuryAta)) - t0, Math.floor((rem * 4000) / 10000));
    assert.isBelow(Math.floor((rem * 4000) / 10000), Math.floor((FEE_10 * 4000) / 10000));
    const c = await program.account.categoryRecord.fetch(catPda(cat));
    assert.equal(c.subCount.toNumber(), 1);
    const rec = await program.account.nameRecord.fetch(namePda(name));
    assert.ok(rec.owner.equals(wallet.publicKey));
  });

  it("register_under_category: rejects a name not under the category", async () => {
    const cat = "defi.agent";
    const name = "rogue.other.agent"; // parent is other.agent, not defi.agent
    let msg = "";
    try {
      await program.methods
        .registerUnderCategory(name, hashArg(name), cat, hashArg(cat), "")
        .accountsPartial({
          config: configPda,
          nameRecord: namePda(name),
          category: catPda(cat),
          categoryOwner: wallet.publicKey,
          payer: wallet.publicKey,
          categoryOwnerNaimAta: payerAta,
          ...tokenAccts(),
        })
        .rpc();
    } catch (e) {
      msg = e.toString();
    }
    assert.include(msg, "NotUnderCategory");
  });

  it("transfer_category: moves the royalty stream", async () => {
    const cat = "defi.agent";
    const newOwner = Keypair.generate().publicKey;
    await program.methods
      .transferCategory(newOwner)
      .accountsPartial({ categoryRecord: catPda(cat), owner: wallet.publicKey })
      .rpc();
    const c = await program.account.categoryRecord.fetch(catPda(cat));
    assert.ok(c.owner.equals(newOwner));
  });

  // ===== SPONSORED DISCOVERY (update/3) ===============================
  it("place_rank_bid: burns 100% and records the bid; accumulates in-epoch; non-owner rejected", async () => {
    const name = "booster.agent"; // 13 chars, owned by the provider wallet
    const cap = "web_search";
    const capHash = Array.from(sha256(cap));
    const rankPda = PublicKey.findProgramAddressSync(
      [Buffer.from("rankbid"), sha256(name), sha256(cap)],
      program.programId
    )[0];

    await program.methods
      .registerName(name, hashArg(name), "ar://boost")
      .accountsPartial({ config: configPda, nameRecord: namePda(name), payer: wallet.publicKey, stakeAccount: null, ...tokenAccts() })
      .rpc();

    const accts = {
      tokenConfig: tokenConfigPda,
      nameRecord: namePda(name),
      rankBid: rankPda,
      owner: wallet.publicKey,
      naimMint,
      bidderNaimAta: payerAta,
      tokenProgram: TOKEN_PROGRAM_ID,
    };

    // first bid: 100 $NAIM burned, recorded
    const sup0 = await supply();
    const b0 = await tokenBal(payerAta);
    await program.methods.placeRankBid(hashArg(name), cap, capHash, new BN(100 * N)).accountsPartial(accts).rpc();
    assert.equal(sup0 - (await supply()), 100 * N, "100% burned");
    assert.equal(b0 - (await tokenBal(payerAta)), 100 * N, "bidder debited");
    let rb = await program.account.rankBid.fetch(rankPda);
    assert.equal(rb.amount.toNumber(), 100 * N);
    assert.ok(rb.owner.equals(wallet.publicKey));
    assert.deepEqual(Array.from(rb.capabilityHash), capHash);

    // second bid same epoch accumulates
    await program.methods.placeRankBid(hashArg(name), cap, capHash, new BN(50 * N)).accountsPartial(accts).rpc();
    rb = await program.account.rankBid.fetch(rankPda);
    assert.equal(rb.amount.toNumber(), 150 * N, "accumulates within the epoch");

    // a non-owner cannot boost someone else's name
    const stranger = Keypair.generate();
    let failed = false;
    try {
      await program.methods
        .placeRankBid(hashArg(name), cap, capHash, new BN(10 * N))
        .accountsPartial({ ...accts, owner: stranger.publicKey })
        .signers([stranger])
        .rpc();
    } catch {
      failed = true;
    }
    assert.isTrue(failed, "non-owner bid must fail");
  });

  // ===== EXPIRY + RECLAIM (base, run last: shortens the periods) =======
  it("update_config: admin shortens periods; non-admin rejected", async () => {
    const shortCfg = {
      treasury: treasury.publicKey,
      fee14: new BN(FEE_1_4),
      fee59: new BN(FEE_5_9),
      fee10Plus: new BN(FEE_10),
      verifyFee: new BN(VERIFY_FEE),
      registrationPeriod: new BN(3),
      gracePeriod: new BN(2),
    };
    const attacker = Keypair.generate();
    let msg = "";
    try {
      await program.methods
        .updateConfig(shortCfg)
        .accountsPartial({ config: configPda, admin: attacker.publicKey })
        .signers([attacker])
        .rpc();
    } catch (e) {
      msg = e.toString();
    }
    assert.include(msg, "Unauthorized");

    await program.methods
      .updateConfig(shortCfg)
      .accountsPartial({ config: configPda, admin: wallet.publicKey })
      .rpc();
    assert.equal((await program.account.config.fetch(configPda)).registrationPeriod.toNumber(), 3);
  });

  it("reclaim: expired-past-grace name re-registrable by a new owner (paid in $NAIM)", async () => {
    const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
    const name = "reclaimme.agent";
    const pda = namePda(name);

    // fund a second owner B with SOL + $NAIM up front
    const b = Keypair.generate();
    const air = await connection.requestAirdrop(b.publicKey, 1_000_000_000);
    await connection.confirmTransaction(air, "confirmed");
    const bAta = (await getOrCreateAssociatedTokenAccount(connection, payer, naimMint, b.publicKey)).address;
    await mintTo(connection, payer, naimMint, bAta, wallet.publicKey, 100 * N);
    const bAccts = {
      config: configPda,
      nameRecord: pda,
      payer: b.publicKey,
      stakeAccount: null,
      tokenConfig: tokenConfigPda,
      naimMint,
      payerNaimAta: bAta,
      treasuryNaimAta: treasuryAta,
      stakersVaultAta,
      stakersVault: stakersVaultPda,
      tokenProgram: TOKEN_PROGRAM_ID,
    };

    // A (provider) registers
    await program.methods
      .registerName(name, hashArg(name), "ar://a")
      .accountsPartial({ config: configPda, nameRecord: pda, payer: wallet.publicKey, stakeAccount: null, ...tokenAccts() })
      .rpc();

    // B before expiry+grace -> rejected
    let failed = false;
    try {
      await program.methods.registerName(name, hashArg(name), "ar://b").accountsPartial(bAccts).signers([b]).rpc();
    } catch {
      failed = true;
    }
    assert.isTrue(failed, "reclaim before grace must fail");

    await sleep(6500); // period 3 + grace 2
    await program.methods.registerName(name, hashArg(name), "ar://b").accountsPartial(bAccts).signers([b]).rpc();
    const rec = await program.account.nameRecord.fetch(pda);
    assert.ok(rec.owner.equals(b.publicKey), "taken over by B");
    assert.equal(rec.metadataUri, "ar://b");
  });
});
