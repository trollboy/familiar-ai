use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum CompressCommand {
    OutputEnable {
        stage: String,
        #[arg(default_value = "compact")]
        register: String,
        #[arg(long)]
        actor: String,
    },
    InputEnable {
        provider: String,
        #[arg(default_value = "native-rle")]
        transform: String,
        #[arg(long)]
        actor: String,
    },
    /// With --lane, auditably label subsequent observations; without it,
    /// report only measured paired ledger values. Default-on promotion
    /// requires a recorded experiment result.
    Experiment {
        label: String,
        #[arg(long)]
        lane: Option<String>,
        #[arg(long, requires = "lane")]
        actor: Option<String>,
    },
}

pub fn compress_command(command: CompressCommand) -> Result<(), String> {
    match command {
        CompressCommand::OutputEnable {
            stage,
            register,
            actor,
        } => crate::compress_cli::configure_output(&stage, &register, &actor),
        CompressCommand::InputEnable {
            provider,
            transform,
            actor,
        } => crate::compress_cli::configure_input(&provider, &transform, &actor),
        CompressCommand::Experiment { label, lane, actor } => match lane {
            Some(lane) => crate::compress_cli::configure_experiment(
                &label,
                &lane,
                actor.as_deref().expect("clap requires actor with lane"),
            ),
            None => crate::compress_cli::experiment(&label),
        },
    }
}
