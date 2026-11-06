use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::{InstructionData, ToAccountMetas};
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_message::Message;
use solana_signer::Signer;
use solana_transaction::Transaction;

use trend_oracle::{accounts, instruction, SEED_TREND, ID};

const PROGRAM_SO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/deploy/trend_oracle.so");

fn setup() -> LiteSVM {
    let mut svm = LiteSVM::new();
    svm.add_program_from_file(ID, PROGRAM_SO)
        .expect("load trend_oracle.so");
    svm
}

fn fund(svm: &mut LiteSVM, pubkey: &Pubkey) {
    svm.airdrop(pubkey, 10_000_000_000).unwrap();
}

fn trend_pda(authority: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_TREND, authority.as_ref()], &ID)
}

fn init_ix(payer: &Pubkey, authority: &Pubkey, trend_snapshot: &Pubkey) -> Instruction {
    let accounts = accounts::Initialize {
        payer: *payer,
        authority: *authority,
        trend_snapshot: *trend_snapshot,
        system_program: anchor_lang::system_program::ID,
    };
    Instruction {
        program_id: ID,
        accounts: accounts.to_account_metas(None),
        data: instruction::Initialize {}.data(),
    }
}

fn publish_ix(
    authority: &Pubkey,
    trend_snapshot: &Pubkey,
    label_hash: [u8; 32],
    label_preview: [u8; 32],
    score: u32,
    cluster_size: u32,
    run_id_hash: [u8; 16],
) -> Instruction {
    let accounts = accounts::PublishTrend {
        authority: *authority,
        trend_snapshot: *trend_snapshot,
    };
    Instruction {
        program_id: ID,
        accounts: accounts.to_account_metas(None),
        data: instruction::PublishTrend {
            label_hash,
            label_preview,
            score,
            cluster_size,
            run_id_hash,
        }
        .data(),
    }
}

fn send(svm: &mut LiteSVM, payer: &Keypair, signers: &[&Keypair], ixs: &[Instruction]) -> Result<(), String> {
    let message = Message::new(ixs, Some(&payer.pubkey()));
    let tx = Transaction::new(signers, message, svm.latest_blockhash());
    svm.send_transaction(tx)
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

fn dummy_label(byte: u8) -> ([u8; 32], [u8; 32]) {
    ([byte; 32], [byte; 32])
}

#[test]
fn initialize_creates_pda_owned_by_authority() {
    let mut svm = setup();
    let payer = Keypair::new();
    let authority = Keypair::new();
    fund(&mut svm, &payer.pubkey());

    let (pda, bump) = trend_pda(&authority.pubkey());
    let ix = init_ix(&payer.pubkey(), &authority.pubkey(), &pda);
    send(&mut svm, &payer, &[&payer, &authority], &[ix]).expect("initialize should succeed");

    let account = svm.get_account(&pda).expect("pda account should exist");
    assert_eq!(account.owner, ID);

    // discriminator (8 bytes) + TrendSnapshot fields; authority is right after the discriminator.
    let stored_authority = Pubkey::try_from(&account.data[8..40]).unwrap();
    assert_eq!(stored_authority, authority.pubkey());
    let stored_bump = account.data[account.data.len() - 1];
    assert_eq!(stored_bump, bump);
}

#[test]
fn reinitialize_is_rejected() {
    let mut svm = setup();
    let payer = Keypair::new();
    let authority = Keypair::new();
    fund(&mut svm, &payer.pubkey());

    let (pda, _bump) = trend_pda(&authority.pubkey());
    let ix = init_ix(&payer.pubkey(), &authority.pubkey(), &pda);
    send(&mut svm, &payer, &[&payer, &authority], &[ix.clone()]).expect("first initialize should succeed");

    let result = send(&mut svm, &payer, &[&payer, &authority], &[ix]);
    assert!(result.is_err(), "second initialize on the same PDA must fail");
}

#[test]
fn publish_trend_updates_snapshot_with_onchain_timestamp() {
    let mut svm = setup();
    let payer = Keypair::new();
    let authority = Keypair::new();
    fund(&mut svm, &payer.pubkey());

    let (pda, _bump) = trend_pda(&authority.pubkey());
    send(
        &mut svm,
        &payer,
        &[&payer, &authority],
        &[init_ix(&payer.pubkey(), &authority.pubkey(), &pda)],
    )
    .expect("initialize should succeed");

    let (label_hash, label_preview) = dummy_label(7);
    let ix = publish_ix(
        &authority.pubkey(),
        &pda,
        label_hash,
        label_preview,
        4_200,
        37,
        [9u8; 16],
    );
    send(&mut svm, &payer, &[&payer, &authority], &[ix]).expect("publish_trend should succeed");

    let account = svm.get_account(&pda).unwrap();
    // score: u32 sits after authority(32) + label_hash(32) + label_preview(32) = offset 8+96=104
    let score = u32::from_le_bytes(account.data[104..108].try_into().unwrap());
    assert_eq!(score, 4_200);
    let published_at = i64::from_le_bytes(account.data[112..120].try_into().unwrap());
    assert!(published_at > 0, "published_at must be stamped on-chain");
}

#[test]
fn publish_trend_rejects_score_above_max() {
    let mut svm = setup();
    let payer = Keypair::new();
    let authority = Keypair::new();
    fund(&mut svm, &payer.pubkey());

    let (pda, _bump) = trend_pda(&authority.pubkey());
    send(
        &mut svm,
        &payer,
        &[&payer, &authority],
        &[init_ix(&payer.pubkey(), &authority.pubkey(), &pda)],
    )
    .expect("initialize should succeed");

    let (label_hash, label_preview) = dummy_label(1);
    let ix = publish_ix(&authority.pubkey(), &pda, label_hash, label_preview, 10_001, 1, [0u8; 16]);
    let result = send(&mut svm, &payer, &[&payer, &authority], &[ix]);
    assert!(result.is_err(), "score above 10_000 bps must be rejected");
}

#[test]
fn publish_trend_rejects_mismatched_authority_pda() {
    let mut svm = setup();
    let payer = Keypair::new();
    let victim = Keypair::new();
    let attacker = Keypair::new();
    fund(&mut svm, &payer.pubkey());
    fund(&mut svm, &attacker.pubkey());

    let (victim_pda, _) = trend_pda(&victim.pubkey());
    let (attacker_pda, _) = trend_pda(&attacker.pubkey());

    send(
        &mut svm,
        &payer,
        &[&payer, &victim],
        &[init_ix(&payer.pubkey(), &victim.pubkey(), &victim_pda)],
    )
    .expect("victim initialize should succeed");
    send(
        &mut svm,
        &payer,
        &[&payer, &attacker],
        &[init_ix(&payer.pubkey(), &attacker.pubkey(), &attacker_pda)],
    )
    .expect("attacker initialize should succeed");

    // Attacker signs as authority but points the trend_snapshot account at the
    // victim's PDA. The seeds constraint derives the expected PDA from the
    // signing authority, so this must fail before `has_one` is even checked.
    let (label_hash, label_preview) = dummy_label(2);
    let malicious = Instruction {
        program_id: ID,
        accounts: vec![
            AccountMeta::new_readonly(attacker.pubkey(), true),
            AccountMeta::new(victim_pda, false),
        ],
        data: instruction::PublishTrend {
            label_hash,
            label_preview,
            score: 1,
            cluster_size: 1,
            run_id_hash: [0u8; 16],
        }
        .data(),
    };

    let result = send(&mut svm, &payer, &[&payer, &attacker], &[malicious]);
    assert!(result.is_err(), "cross-authority publish must be rejected");
}
