use anyhow::bail;
use anyhow::Result;
use clap::{Parser, Subcommand};
use contract::Token;
use hyle::model::BlobTransaction;
use hyle::model::ProofData;
use hyle::model::ProofTransaction;
use hyle::model::RegisterContractTransaction;
use risc0_zkvm::Receipt;
use risc0_zkvm::{default_prover, ExecutorEnv};
use sdk::HyleOutput;
use sdk::{ContractInput, Digestable};

// These constants represent the RISC-V ELF and the image ID generated by risc0-build.
// The ELF is used for proving and the ID is used for verification.
use methods::{GUEST_ELF, GUEST_ID};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[clap(long, short)]
    reproducible: bool,

    #[arg(long, default_value = "http://localhost:4321")]
    pub host: String,

    #[arg(long, default_value = "simple_token")]
    pub contract_name: String,
}

#[derive(Subcommand)]
enum Commands {
    Register {
        supply: u128,
    },
    Transfer {
        from: String,
        to: String,
        amount: u128,
    },
}

#[tokio::main]
async fn main() {
    // Initialize tracing. In order to view logs, run `RUST_LOG=info cargo run`
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::filter::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    if cli.reproducible {
        println!("Running with reproducible ELF binary.");
    } else {
        println!("Running non-reproducibly");
    }

    let client = hyle::tools::rest_api_client::ApiHttpClient::new(cli.host);

    let contract_name = &cli.contract_name;

    match cli.command {
        Commands::Register { supply } => {
            let initial_state = Token::new(supply, format!("faucet.{}", contract_name).into());

            println!("Initial state: {:?}", initial_state);
            let initial_state = initial_state.as_digest();

            let res = client
                .send_tx_register_contract(&RegisterContractTransaction {
                    owner: "examples".to_string(),
                    verifier: "risc0".into(),
                    program_id: sdk::ProgramId(sdk::to_u8_array(&GUEST_ID).to_vec()),
                    state_digest: initial_state,
                    contract_name: contract_name.clone().into(),
                })
                .await
                .unwrap();
            println!(
                "✅ Register contract tx sent. Tx hash: {}",
                res.text().await.unwrap()
            );
        }
        Commands::Transfer { from, to, amount } => {
            let initial_state: Token = client
                .get_contract(&contract_name.clone().into())
                .await
                .unwrap()
                .state
                .into();

            let action = sdk::erc20::ERC20Action::Transfer {
                recipient: to.clone(),
                amount,
            };

            let blobs = vec![sdk::Blob {
                contract_name: contract_name.clone().into(),
                data: sdk::BlobData(
                    bincode::encode_to_vec(action, bincode::config::standard())
                        .expect("failed to encode BlobData"),
                ),
            }];

            let inputs = ContractInput::<Token> {
                initial_state,
                identity: from.clone().into(),
                tx_hash: "".into(),
                private_blob: sdk::BlobData(vec![]),
                blobs: blobs.clone(),
                index: sdk::BlobIndex(0),
            };

            let receipt = prove(cli.reproducible, inputs).unwrap();

            let blob_tx_hash = client
                .send_tx_blob(&BlobTransaction {
                    identity: from.into(),
                    blobs,
                })
                .await
                .unwrap();
            println!("✅ Blob tx sent. Tx hash: {}", blob_tx_hash);

            let proof_tx_hash = client
                .send_tx_proof(&ProofTransaction {
                    blob_tx_hash,
                    proof: ProofData::Bytes(
                        borsh::to_vec(&receipt).expect("Unable to encode receipt"),
                    ),
                    contract_name: contract_name.clone().into(),
                })
                .await
                .unwrap();
            println!(
                "✅ Proof tx sent. Tx hash: {}",
                proof_tx_hash.text().await.unwrap()
            );
        }
    }
}

fn prove(reproducible: bool, input: ContractInput<Token>) -> Result<Receipt> {
    let env = ExecutorEnv::builder()
        .write(&input)
        .unwrap()
        .build()
        .unwrap();

    let prover = default_prover();
    let binary = if reproducible {
        std::fs::read("target/riscv-guest/riscv32im-risc0-zkvm-elf/docker/method/method")
            .expect("Could not read ELF binary at target/riscv-guest/riscv32im-risc0-zkvm-elf/docker/method/method")
    } else {
        GUEST_ELF.to_vec()
    };
    let receipt = prover.prove(env, &binary).unwrap().receipt;

    let hyle_output = receipt
        .journal
        .decode::<HyleOutput>()
        .expect("Failed to decode journal");

    if !hyle_output.success {
        let program_error = std::str::from_utf8(&hyle_output.program_outputs).unwrap();
        println!(
            "\x1b[91mExecution failed ! Program output: {}\x1b[0m",
            program_error
        );
        bail!("Execution failed");
    }
    Ok(receipt)
}
