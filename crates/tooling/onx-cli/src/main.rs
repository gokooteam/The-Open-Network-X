use clap::Parser;
use onx_cli::{
    build_transfer, chain_id_from_genesis, load_seed_file, load_wallet_key, parse_hex32,
    wallet_create, write_to_pool, Cli, Command, TransferArgs, TransferRequest, WalletCommand,
};
use onx_data_structures::AccountId;
use onx_stf::derive_address;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Wallet { kind } => match kind {
            WalletCommand::Create { out } => wallet_create(&out).map(|w| {
                println!("wallet={}", out.join("wallet.json").display());
                println!("mnemonic={}", w.mnemonic.join(" "));
                println!("public_key={}", w.public_key_hex);
                println!("address={}", w.address_hex);
            }),
            WalletCommand::Address { wallet } => load_wallet_key(&wallet).map(|key| {
                let pk = key.public_key().encode();
                println!("public_key={}", hex::encode(pk));
                println!("address={}", hex::encode(derive_address(&pk).to_bytes()));
            }),
        },
        Command::Transfer(args) => transfer(args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("onx-cli: {err}");
            ExitCode::FAILURE
        }
    }
}

fn transfer(args: TransferArgs) -> Result<(), String> {
    let chain_id = match (&args.genesis, &args.chain_id) {
        (Some(path), None) => chain_id_from_genesis(path)?,
        (None, Some(hex)) => parse_hex32(hex, "--chain-id")?,
        _ => return Err("exactly one of --genesis / --chain-id is required".into()),
    };
    let secret = match (&args.wallet, &args.seed_file) {
        (Some(path), None) => load_wallet_key(path)?,
        (None, Some(path)) => load_seed_file(path)?,
        _ => return Err("exactly one of --wallet / --seed-file is required".into()),
    };
    let from = args
        .from
        .as_deref()
        .map(|s| parse_hex32(s, "--from").map(AccountId::from_bytes))
        .transpose()?;
    let req = TransferRequest {
        chain_id,
        from,
        to: AccountId::from_bytes(parse_hex32(&args.to, "--to")?),
        amount_nanos: args.amount,
        fee_nanos: args.fee,
        nonce: args.nonce,
        reveal_key: args.reveal_key,
    };
    let msg = build_transfer(&req, &secret)?;
    println!("chain_id={}", hex::encode(chain_id));
    println!("from={}", hex::encode(msg.from.to_bytes()));
    println!("hash={}", hex::encode(msg.hash()));
    match &args.out {
        Some(dir) => println!("file={}", write_to_pool(dir, &msg)?.display()),
        None => println!("msg={}", hex::encode(msg.to_bytes())),
    }
    Ok(())
}
