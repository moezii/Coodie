//! The description pass, against a real model.
//!
//! Unit tests cover the plumbing — clustering, prompt shape, citation
//! verification, parsing. None of that proves the model is being asked a
//! question it can answer, which is the part that silently degrades: a prompt
//! change can leave every test green and every description useless.
//!
//! These run only when a provider is configured. Without a key they skip rather
//! than fail, so the suite still passes on a machine with no credentials.

use codereader::audit::{audit_repository, describe_report};
use codereader::llm::provider_from_env;
use std::fs;
use std::path::Path;
use tempfile::{tempdir, TempDir};

/// A service whose behaviour is unambiguous, so the description can be checked
/// against something rather than merely being non-empty.
fn booking_service() -> TempDir {
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    fs::create_dir_all(root.join("app")).expect("dirs");
    fs::write(
        root.join("app/main.py"),
        r#"
from fastapi import FastAPI

app = FastAPI()


@app.post("/bookings")
def create_booking(car_id: str):
    """Creates a booking for a car and returns its reference."""
    return {"reference": reserve_car(car_id)}


@app.delete("/bookings/{reference}")
def cancel_booking(reference: str):
    """Cancels an existing booking."""
    return release_car(reference)


def reserve_car(car_id):
    return "BK-1"


def release_car(reference):
    return True
"#,
    )
    .expect("write");
    fs::write(
        root.join("README.md"),
        "# Fleet\n\nBook and cancel car bookings.\n\n\
         Administrators can export a monthly revenue report as a CSV file.\n",
    )
    .expect("write");
    directory
}

/// Build a provider, or `None` when the machine has no credentials.
fn provider_or_skip(root: &Path) -> Option<Box<dyn codereader::llm::LlmProvider>> {
    match provider_from_env(root, None) {
        Ok(provider) => Some(provider),
        Err(_) => {
            eprintln!("skipping: no LLM configured");
            None
        }
    }
}

#[test]
fn describes_what_the_service_actually_does() {
    let directory = booking_service();
    let mut report = audit_repository(directory.path()).expect("audit");
    // The key lives with the operator, not in the audited repository.
    let Some(mut provider) = provider_or_skip(Path::new(".")) else {
        return;
    };

    describe_report(&mut report, provider.as_mut(), None).expect("describe");
    let narrative = report.narrative.as_ref().expect("narrative");

    assert!(
        !narrative.features.is_empty(),
        "no feature was described at all"
    );
    let prose = narrative
        .features
        .iter()
        .map(|feature| feature.summary.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");

    // The description has to name the domain, not restate the file path.
    // Concept, not one token: a model may say "reservation". It must still
    // name the domain rather than restate the file path.
    assert!(
        prose.contains("book") || prose.contains("reserv"),
        "a booking service was not described as one: {prose}"
    );
    assert!(
        prose.contains("cancel") || prose.contains("delete") || prose.contains("remove"),
        "cancellation is half the service and was not mentioned: {prose}"
    );

    // Every citation must exist. An invented id would have been dropped.
    let ids: Vec<&str> = report.graph.nodes.iter().map(|n| n.id.as_str()).collect();
    for feature in &narrative.features {
        for cited in &feature.cited_nodes {
            assert!(
                ids.contains(&cited.as_str()),
                "cited a node that does not exist: {cited}"
            );
        }
    }
}

#[test]
fn catches_a_documented_feature_that_does_not_exist() {
    // The README promises a CSV revenue export. Nothing in the code exports
    // anything. If this stops being detected, the claim check has silently
    // become decoration.
    let directory = booking_service();
    let mut report = audit_repository(directory.path()).expect("audit");
    let Some(mut provider) = provider_or_skip(Path::new(".")) else {
        return;
    };

    describe_report(&mut report, provider.as_mut(), None).expect("describe");
    let narrative = report.narrative.as_ref().expect("narrative");

    let promises = narrative
        .unfulfilled_promises
        .iter()
        .map(|promise| format!("{} {}", promise.promise, promise.reason).to_lowercase())
        .collect::<Vec<_>>()
        .join(" | ");

    assert!(
        promises.contains("csv") || promises.contains("export") || promises.contains("revenue"),
        "the unimplemented CSV export was not reported: {promises:?}"
    );
}

#[test]
fn a_service_that_keeps_its_promises_reports_none() {
    // The mirror of the test above: no invented findings when the docs are
    // honest. A checker that always finds something is as useless as one that
    // never does.
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    fs::create_dir_all(root.join("app")).expect("dirs");
    fs::write(
        root.join("app/main.py"),
        "from fastapi import FastAPI\n\napp = FastAPI()\n\n\
         @app.get(\"/health\")\ndef health():\n    \"\"\"Reports whether the service is running.\"\"\"\n    return {\"ok\": True}\n",
    )
    .expect("write");
    fs::write(
        root.join("README.md"),
        "# Health\n\nA service with a single health check endpoint.\n",
    )
    .expect("write");

    let mut report = audit_repository(root).expect("audit");
    let Some(mut provider) = provider_or_skip(Path::new(".")) else {
        return;
    };

    describe_report(&mut report, provider.as_mut(), None).expect("describe");
    let narrative = report.narrative.as_ref().expect("narrative");
    // The model is not deterministic, so this asserts the property that
    // matters rather than an exact count: whatever it says, it must not claim
    // the one documented feature — the health endpoint — is missing when it
    // plainly exists in the inventory.
    let invented = narrative
        .unfulfilled_promises
        .iter()
        .map(|promise| promise.promise.to_lowercase())
        .find(|promise| promise.contains("health"));
    assert!(
        invented.is_none(),
        "claimed an implemented feature was missing: {invented:?}"
    );
    assert!(
        narrative.unfulfilled_promises.len() <= 1,
        "an honest repository should not produce a list of problems: {:?}",
        narrative.unfulfilled_promises
    );
}
