//! Pure consent decisions over bounded, real owner plans; no executor is implied.
use docsbase_memory::config::bootstrap::{Entrypoint, EntrypointFacts};
use docsbase_memory::config::change::{
    Action, Assessment, BlockerCode, Effect, ExpectedState, Grant, Observation, PlanLimits, Reason,
    ResourceId, Target,
};
use docsbase_memory::config::policy::{
    ConsentDecision, ConsentRefusal, ConsentReview, GrantSet, InvalidationPolicy, Policy,
    PolicyContext, PolicyMode, ResolvedPolicy, resolve,
};
use docsbase_memory::config::schema::{ArtifactId, SourceId};
use docsbase_memory::config::source::SourceLayer;
use docsbase_memory::search::settings::SearchSpec;

fn limits() -> PlanLimits {
    PlanLimits {
        max_actions: 16,
        max_dependencies: 32,
        max_metadata_bytes: 32 * 1024,
    }
}
fn policy(request: InvalidationPolicy, entrypoint: Entrypoint, tty: bool) -> ResolvedPolicy {
    let PolicyContext::Managed(resolved) = resolve(
        PolicyMode::Managed,
        Some(request),
        EntrypointFacts {
            mode: entrypoint,
            is_tty: tty,
        },
    ) else {
        panic!("explicit managed policy");
    };
    resolved
}
fn rebuild(grants: Vec<Grant>) -> Assessment {
    let resource = ResourceId::Artifact(ArtifactId {
        component: "lexical_index".into(),
        project_id: Some(1),
    });
    Assessment {
        expected: vec![Observation {
            resource: resource.clone(),
            state: ExpectedState::KnownIdentity("lexical-generation-v1:old".into()),
        }],
        actions: vec![Action {
            resource,
            effect: Effect::Rebuild,
            target: Some(Target::Identity("lexical-recipe-v1:new".into())),
            reasons: vec![Reason::SettingsChanged],
            dependencies: vec![],
            required_grants: grants,
        }],
        blockers: vec![],
    }
}
fn receipt(decision: ConsentDecision) -> docsbase_memory::config::policy::ConsentReceipt {
    match decision {
        ConsentDecision::ConsentSatisfied(receipt) => receipt,
        other => panic!("expected satisfied consent, got {other:?}"),
    }
}

#[test]
fn launch_resolution_preserves_legacy_and_captures_auto_once() {
    let entries = [
        (Entrypoint::Cli, false, Policy::Fail),
        (Entrypoint::Cli, true, Policy::Prompt),
        (Entrypoint::Service, false, Policy::Fail),
        (Entrypoint::Service, true, Policy::Fail),
        (Entrypoint::Mcp, false, Policy::Fail),
        (Entrypoint::Mcp, true, Policy::Fail),
        (Entrypoint::Autostart, false, Policy::Fail),
        (Entrypoint::Autostart, true, Policy::Fail),
        (Entrypoint::Standalone, false, Policy::Fail),
        (Entrypoint::Standalone, true, Policy::Fail),
    ];
    for (mode, is_tty, expected) in entries {
        let facts = EntrypointFacts { mode, is_tty };
        assert_eq!(
            resolve(PolicyMode::LegacyV1, None, facts),
            PolicyContext::Legacy
        );
        let PolicyContext::Managed(auto) = resolve(PolicyMode::Managed, None, facts) else {
            panic!("managed context");
        };
        assert_eq!(auto.policy(), expected);
        for (request, expected) in [
            (InvalidationPolicy::Auto, expected),
            (InvalidationPolicy::Prompt, Policy::Prompt),
            (InvalidationPolicy::Apply, Policy::Apply),
            (InvalidationPolicy::Fail, Policy::Fail),
        ] {
            let PolicyContext::Managed(explicit) =
                resolve(PolicyMode::LegacyV1, Some(request), facts)
            else {
                panic!("explicit policy opts into managed boundary");
            };
            assert_eq!(explicit.policy(), expected);
        }
    }
}

#[test]
fn runtime_initialization_and_restart_need_no_consent_but_keep_executor_blockers() {
    let settings = docsbase_memory::search::settings::standalone()
        .unwrap()
        .into_settings();
    let mut changed = settings.clone();
    changed.request_timeout_ms += 1;
    let db = docsbase_memory::store::settings::standalone()
        .unwrap()
        .into_settings();
    let mut db_changed = db.clone();
    db_changed.read_busy_timeout += std::time::Duration::from_millis(1);
    let assessments = [
        SearchSpec::assess_change(None, &settings).unwrap(),
        SearchSpec::assess_change(Some(&settings), &changed).unwrap(),
        SearchSpec::assess_change(Some(&settings), &settings).unwrap(),
        docsbase_memory::store::settings::DbSpec::assess_change(None, &db).unwrap(),
        docsbase_memory::store::settings::DbSpec::assess_change(Some(&db), &db_changed).unwrap(),
        docsbase_memory::store::settings::DbSpec::assess_change(Some(&db), &db).unwrap(),
    ];
    for assessment in assessments {
        let before = assessment.clone();
        let review = ConsentReview::compile(std::slice::from_ref(&assessment), limits()).unwrap();
        for request in [
            InvalidationPolicy::Auto,
            InvalidationPolicy::Prompt,
            InvalidationPolicy::Apply,
            InvalidationPolicy::Fail,
        ] {
            for mode in [
                Entrypoint::Cli,
                Entrypoint::Service,
                Entrypoint::Mcp,
                Entrypoint::Autostart,
                Entrypoint::Standalone,
            ] {
                receipt(review.evaluate(policy(request, mode, false), GrantSet::empty()))
                    .revalidate(&review)
                    .unwrap();
            }
        }
        assert_eq!(assessment, before);
        assert_eq!(
            review.plan().body.blockers.len(),
            review.plan().body.actions.len()
        );
        assert!(
            review
                .plan()
                .body
                .blockers
                .iter()
                .all(|b| b.code == BlockerCode::ExecutorUnavailable)
        );
    }
}

#[test]
fn rebuild_and_noninitial_effects_require_consent_even_with_grants() {
    let all = GrantSet::new(true, true, true);
    for effect in [
        Effect::Rebuild,
        Effect::Evict,
        Effect::DeleteStore,
        Effect::MigrateSchema,
        Effect::RecoverStore,
        Effect::RewriteSource,
        Effect::AdoptSharedStore,
    ] {
        let mut assessment = rebuild(vec![]);
        assessment.actions[0].effect = effect;
        let review = ConsentReview::compile(&[assessment], limits()).unwrap();
        let refused = review.evaluate(policy(InvalidationPolicy::Fail, Entrypoint::Cli, true), all);
        assert_eq!(refused.refusal(), Some(ConsentRefusal::RequiredChange));
        assert_eq!(refused.refusal().unwrap().exit_code(), 3);
    }
    // First store creation is safe only with an observed absence and no grants.
    let mut init = rebuild(vec![]);
    init.actions[0].effect = Effect::InitializeStore;
    init.expected[0].state = ExpectedState::Missing;
    let review = ConsentReview::compile(&[init.clone()], limits()).unwrap();
    receipt(review.evaluate(
        policy(InvalidationPolicy::Fail, Entrypoint::Mcp, false),
        GrantSet::empty(),
    ));
    init.expected[0].state = ExpectedState::Unknown;
    let review = ConsentReview::compile(&[init.clone()], limits()).unwrap();
    assert_eq!(
        review
            .evaluate(policy(InvalidationPolicy::Fail, Entrypoint::Cli, true), all)
            .refusal(),
        Some(ConsentRefusal::RequiredChange)
    );
    init.expected[0].state = ExpectedState::Missing;
    init.actions[0].required_grants = vec![Grant::Downloads];
    let review = ConsentReview::compile(&[init], limits()).unwrap();
    assert_eq!(
        review
            .evaluate(policy(InvalidationPolicy::Fail, Entrypoint::Cli, true), all)
            .refusal(),
        Some(ConsentRefusal::RequiredChange)
    );
}

#[test]
fn apply_checks_each_grant_and_never_authorizes_service_loop_rebuilds() {
    let review = ConsentReview::compile(
        &[rebuild(vec![
            Grant::Remote,
            Grant::Downloads,
            Grant::Destructive,
        ])],
        limits(),
    )
    .unwrap();
    let apply = policy(InvalidationPolicy::Apply, Entrypoint::Cli, false);
    for grants in [
        GrantSet::empty(),
        GrantSet::new(false, true, true),
        GrantSet::new(true, false, true),
        GrantSet::new(true, true, false),
    ] {
        assert_eq!(
            review.evaluate(apply, grants).refusal(),
            Some(ConsentRefusal::MissingGrants)
        );
    }
    receipt(review.evaluate(apply, GrantSet::new(true, true, true)))
        .revalidate(&review)
        .unwrap();
    let local = ConsentReview::compile(&[rebuild(vec![])], limits()).unwrap();
    receipt(local.evaluate(apply, GrantSet::empty()));
    for mode in [
        Entrypoint::Service,
        Entrypoint::Mcp,
        Entrypoint::Autostart,
        Entrypoint::Standalone,
    ] {
        for tty in [false, true] {
            for request in [InvalidationPolicy::Prompt, InvalidationPolicy::Apply] {
                assert_eq!(
                    review
                        .evaluate(policy(request, mode, tty), GrantSet::new(true, true, true))
                        .refusal(),
                    Some(ConsentRefusal::NoControllingCli)
                );
            }
        }
    }
    assert_eq!(
        review
            .evaluate(
                policy(InvalidationPolicy::Prompt, Entrypoint::Cli, false),
                GrantSet::new(true, true, true)
            )
            .refusal(),
        Some(ConsentRefusal::NoTerminal)
    );
}

#[test]
fn prompt_answer_binds_displayed_plan_and_unions_only_explicit_grants() {
    let review = ConsentReview::compile(
        &[rebuild(vec![
            Grant::Remote,
            Grant::Downloads,
            Grant::Destructive,
        ])],
        limits(),
    )
    .unwrap();
    let ConsentDecision::PromptRequired(prompt) = review.evaluate(
        policy(InvalidationPolicy::Auto, Entrypoint::Cli, true),
        GrantSet::new(true, false, false),
    ) else {
        panic!("interactive direct auto asks before rebuilding");
    };
    assert_eq!(prompt.plan_id(), review.plan().plan_id);
    assert_eq!(
        prompt
            .answer(prompt.plan_id(), false, GrantSet::new(true, true, true))
            .unwrap_err(),
        ConsentRefusal::Declined
    );
    assert_eq!(
        prompt
            .answer(prompt.plan_id(), true, GrantSet::new(false, true, false))
            .unwrap_err(),
        ConsentRefusal::MissingGrants
    );
    assert_eq!(
        prompt
            .answer("another-plan", true, GrantSet::new(true, true, true))
            .unwrap_err(),
        ConsentRefusal::StalePlan
    );
    assert_eq!(ConsentRefusal::StalePlan.exit_code(), 4);
    prompt
        .answer(prompt.plan_id(), true, GrantSet::new(false, true, true))
        .unwrap()
        .revalidate(&review)
        .unwrap();
    assert_eq!(
        review.plan().body.blockers[0].code,
        BlockerCode::ExecutorUnavailable
    );
}

#[test]
fn consent_receipt_expires_when_observed_identity_or_grants_change() {
    let assessment = rebuild(vec![]);
    let review = ConsentReview::compile(std::slice::from_ref(&assessment), limits()).unwrap();
    let receipt = receipt(review.evaluate(
        policy(InvalidationPolicy::Apply, Entrypoint::Cli, false),
        GrantSet::empty(),
    ));
    let same = ConsentReview::compile(std::slice::from_ref(&assessment), limits()).unwrap();
    receipt.revalidate(&same).unwrap();
    let mut changed = assessment.clone();
    changed.expected[0].state = ExpectedState::KnownIdentity("lexical-generation-v1:new".into());
    let newer = ConsentReview::compile(&[changed], limits()).unwrap();
    assert_eq!(
        receipt.revalidate(&newer).unwrap_err(),
        ConsentRefusal::StalePlan
    );
    let mut changed = assessment;
    changed.actions[0].required_grants.push(Grant::Remote);
    let expensive = ConsentReview::compile(&[changed], limits()).unwrap();
    assert_eq!(
        receipt.revalidate(&expensive).unwrap_err(),
        ConsentRefusal::StalePlan
    );
}

#[test]
fn consent_does_not_remove_migration_blockers_or_relax_compiler_limits() {
    use docsbase_memory::config::migrate::{VectorBudgetDecision, draft_migration};
    let source = SourceLayer::parse(
        SourceId::Global("/config/config.toml".into()),
        "hybrid=false\n",
    )
    .unwrap();
    let draft = draft_migration(
        &source,
        &[],
        &[],
        Some(VectorBudgetDecision::UseBytes(1024)),
    )
    .unwrap();
    let assessment = draft.assess_change().unwrap();
    let review = ConsentReview::compile(std::slice::from_ref(&assessment), limits()).unwrap();
    let before = review.plan().clone();
    receipt(review.evaluate(
        policy(InvalidationPolicy::Apply, Entrypoint::Cli, false),
        GrantSet::new(false, false, true),
    ))
    .revalidate(&review)
    .unwrap();
    assert_eq!(review.plan(), &before);
    assert!(
        review
            .plan()
            .body
            .blockers
            .iter()
            .any(|b| b.code == BlockerCode::StrictStoreAdoptionUnavailable)
    );
    assert!(
        review
            .plan()
            .body
            .blockers
            .iter()
            .any(|b| b.code == BlockerCode::SourceObservationRequired)
    );
    let mut tiny = limits();
    tiny.max_actions = 0;
    assert!(ConsentReview::compile(std::slice::from_ref(&assessment), tiny).is_err());
    tiny = limits();
    tiny.max_metadata_bytes = 1;
    assert!(ConsentReview::compile(&[assessment], tiny).is_err());
    let mut cycle = rebuild(vec![]);
    let self_dependency = cycle.actions[0].resource.clone();
    cycle.actions[0].dependencies.push(self_dependency);
    assert!(ConsentReview::compile(&[cycle], limits()).is_err());
    for failure in [
        ConsentRefusal::NoControllingCli,
        ConsentRefusal::NoTerminal,
        ConsentRefusal::MissingGrants,
        ConsentRefusal::Declined,
    ] {
        assert_eq!(failure.exit_code(), 5);
    }
}
