use anyhow::{Context, Result};
use chrono::Utc;

use crate::job::JobClient;
use crate::job::client::{JobConclusion, ResultsConclusion, ResultsStatus, ResultsStep};
use crate::job::schema::JobManifest;
use crate::utils::format_results_timestamp;

/// Report a setup failure to GitHub with a visible error log.
///
/// Creates a synthetic "Failure" step so the error message appears in the
/// GitHub Actions UI (job-level logs alone are invisible without at least one step).
pub async fn report_setup_failure(
    job_client: &JobClient,
    manifest: &JobManifest,
    err: &anyhow::Error,
) -> Result<()> {
    let plan_id = &manifest.plan.plan_id;
    let job_id = &manifest.plan.job_id;
    let now = format_results_timestamp(Utc::now());
    let step_id = uuid::Uuid::new_v4().to_string();

    // 1. Register a synthetic "Failure" step
    let step = ResultsStep {
        external_id: step_id.clone(),
        number: 1,
        name: "Failure".into(),
        status: ResultsStatus::Completed,
        started_at: Some(now.clone()),
        completed_at: Some(now),
        conclusion: ResultsConclusion::Failure,
    };
    job_client
        .update_steps(plan_id, job_id, &[step])
        .await
        .context("registering synthetic setup step")?;

    // 2. Upload the error log to the step's blob
    let error_log = format_setup_error_log(err);
    let line_count = error_log.lines().count() as i64;

    let signed = job_client
        .get_step_log_signed_url(plan_id, job_id, &step_id)
        .await
        .context("getting step log signed URL")?;

    job_client
        .create_append_blob(&signed)
        .await
        .context("creating step log blob")?;
    job_client
        .append_blob_block(&signed, &error_log)
        .await
        .context("writing step log content")?;
    job_client
        .seal_blob(&signed)
        .await
        .context("sealing step log blob")?;

    job_client
        .create_step_log_metadata(plan_id, job_id, &step_id, line_count)
        .await
        .context("posting step log metadata")?;

    // 3. Complete the job as failed
    job_client
        .complete_job(
            plan_id,
            job_id,
            JobConclusion::Failed,
            &serde_json::json!({}),
            &[],
        )
        .await
        .context("completing job as failed")?;

    Ok(())
}

/// Format an anyhow error chain into a log block visible in the GitHub UI.
fn format_setup_error_log(err: &anyhow::Error) -> String {
    let ts = crate::utils::format_log_timestamp(Utc::now());
    let r = "\x1b[31m"; // red
    let y = "\x1b[33m"; // yellow
    let reset = "\x1b[0m";
    let version = env!("CARGO_PKG_VERSION");

    let mut lines = Vec::new();
    lines.push(format!(
        "{ts} {y}chimera v{version} — job setup failed{reset}"
    ));
    lines.push(format!("{ts} {r}Error: {err}{reset}"));
    for cause in err.chain().skip(1) {
        lines.push(format!("{ts} {r}  caused by: {cause}{reset}"));
    }
    lines.push(String::new());
    lines.join("\n")
}

/// Convert job outputs to VariableValue dictionary format for the completejob API.
pub fn outputs_to_variable_values(
    outputs: &std::collections::HashMap<String, String>,
) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in outputs {
        map.insert(k.clone(), serde_json::json!({"value": v}));
    }
    serde_json::Value::Object(map)
}
