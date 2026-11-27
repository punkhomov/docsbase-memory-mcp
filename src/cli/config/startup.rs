//! Controlling CLI for startup replacements. The service loop never prompts.

use std::io::{self, IsTerminal, Write};
use std::path::Path;

use crate::config::{
    bootstrap::{Entrypoint, EntrypointFacts},
    change::Grant,
    policy::{
        self, ConsentDecision, ConsentRefusal, ConsentReview, GrantSet, InvalidationPolicy,
        PolicyContext, PolicyMode, ResolvedPolicy,
    },
};
use crate::daemon::launch::{LaunchConfig, preflight, restart_notice};

use super::{CheckStatus, ProjectPaths, apply};

/// Startup policy flags and the grant scoped to this invocation.
#[derive(Debug, Clone, Copy, clap::Args)]
pub struct StartupOptions {
    /// How to handle required index replacements before starting the daemon.
    #[arg(long, value_enum)]
    on_invalidation: Option<StartupPolicy>,
    /// Permit possible remote embedding costs for approved startup replacements.
    #[arg(long)]
    allow_remote: bool,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum StartupPolicy {
    Prompt,
    Apply,
    Fail,
}

impl From<StartupPolicy> for InvalidationPolicy {
    fn from(value: StartupPolicy) -> Self {
        match value {
            StartupPolicy::Prompt => Self::Prompt,
            StartupPolicy::Apply => Self::Apply,
            StartupPolicy::Fail => Self::Fail,
        }
    }
}

pub(in crate::cli) fn prepare(
    cache: &Path,
    launch: &mut LaunchConfig,
    detached: bool,
    options: StartupOptions,
) -> anyhow::Result<()> {
    let managed = if launch.config.settings().vector_store.is_some() {
        PolicyMode::Managed
    } else {
        PolicyMode::LegacyV1
    };
    let facts = EntrypointFacts {
        mode: if detached {
            Entrypoint::Autostart
        } else {
            Entrypoint::Cli
        },
        is_tty: io::stdin().is_terminal() && io::stderr().is_terminal(),
    };
    let requested = options.on_invalidation.map(Into::into);
    let PolicyContext::Managed(policy) = policy::resolve(managed, requested, facts) else {
        return Ok(());
    };
    if requested.is_none() {
        preflight::recover_abandoned_registry(cache, launch)
            .map_err(|error| anyhow::Error::from(error).context(CheckStatus::Unavailable))?;
    }
    let observe = || {
        preflight::survey(cache, launch, requested.is_some())
            .map_err(|error| anyhow::Error::from(error).context(CheckStatus::Unavailable))
    };
    let plans = observe()?;
    verify(&plans, launch)?;
    if plans.replacements.is_empty() {
        launch.invalidation = PolicyContext::Managed(policy);
        launch.startup = Some(plans.expectations);
        return Ok(());
    }
    let mut approved = Vec::with_capacity(plans.replacements.len());
    for (root, plan) in &plans.replacements {
        let review = ConsentReview::from_compiled(plan.clone());
        let allow_remote = authorize(root, &review, policy, options.allow_remote)?;
        approved.push((root.clone(), review.plan().plan_id.clone(), allow_remote));
    }
    verify(&plans, launch)?;
    let settings = crate::store::settings::db_from_config(&launch.config)?;
    for (root, id, allow_remote) in approved {
        verify_global(launch)?;
        apply::execute(
            ProjectPaths {
                config: plans.config.clone(),
                cache: plans.expectations.cache.clone(),
                root,
            },
            &id,
            allow_remote,
            &settings,
            Some(&launch.config.settings().daemon.profile),
        )?;
    }
    let final_plans = observe()?;
    if !final_plans.replacements.is_empty() {
        return Err(CheckStatus::Stale.into());
    }
    verify(&final_plans, launch)?;
    launch.invalidation = PolicyContext::Managed(policy);
    launch.startup = Some(final_plans.expectations);
    Ok(())
}

fn verify(plans: &preflight::StartupPlans, launch: &LaunchConfig) -> anyhow::Result<()> {
    if plans.expectations.verify(launch)? {
        Ok(())
    } else {
        Err(CheckStatus::Stale.into())
    }
}

fn verify_global(launch: &LaunchConfig) -> Result<(), CheckStatus> {
    if restart_notice(launch.path.as_deref(), &launch.stamp).is_some() {
        Err(CheckStatus::Stale)
    } else {
        Ok(())
    }
}

fn authorize(
    root: &Path,
    review: &ConsentReview,
    policy: ResolvedPolicy,
    allow_remote: bool,
) -> anyhow::Result<bool> {
    let root = root.to_string_lossy();
    let root = root.escape_debug();
    let supplied = GrantSet::new(allow_remote, false, false);
    match review.evaluate(policy, supplied) {
        ConsentDecision::ConsentSatisfied(_) => Ok(allow_remote),
        ConsentDecision::Refused(refusal) => {
            Err(anyhow::Error::new(status(refusal)).context(format!("startup plan for {root}")))
        }
        ConsentDecision::PromptRequired(request) => {
            eprintln!(
                "Configuration plan for {root}\n{}",
                serde_json::to_string_pretty(review.plan())?
            );
            let accepted = confirm("Apply this plan before starting the daemon?")?;
            let additional_remote = accepted
                && !allow_remote
                && review.plan().body.required_grants.contains(&Grant::Remote)
                && confirm("Allow remote embedding requests that may incur costs?")?;
            request
                .answer(
                    request.plan_id(),
                    accepted,
                    GrantSet::new(additional_remote, false, false),
                )
                .map_err(status)?;
            Ok(allow_remote || additional_remote)
        }
    }
}

fn confirm(question: &str) -> anyhow::Result<bool> {
    let mut stderr = io::stderr().lock();
    write!(stderr, "{question} [y/N] ")?;
    stderr.flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn status(refusal: ConsentRefusal) -> CheckStatus {
    match refusal {
        ConsentRefusal::RequiredChange => CheckStatus::Required,
        ConsentRefusal::StalePlan => CheckStatus::Stale,
        ConsentRefusal::NoControllingCli | ConsentRefusal::NoTerminal => {
            CheckStatus::PromptUnavailable
        }
        ConsentRefusal::MissingGrants => CheckStatus::MissingGrants,
        ConsentRefusal::Declined => CheckStatus::Declined,
    }
}
