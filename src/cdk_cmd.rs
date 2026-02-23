//! AWS CDK CLI output compression.
//!
//! Filters verbose CloudFormation templates, diff tables, and deploy logs.
//! Specialized handlers for: synth, diff, deploy, destroy, ls.

use crate::tracking;
use anyhow::{Context, Result};
use lazy_static::lazy_static;
use regex::Regex;
use std::process::Command;

lazy_static! {
    static ref CDK_RESOURCE_CHANGE: Regex =
        Regex::new(r"^\[([+\-~])\] (\S+) (\S+)").unwrap();
    static ref CDK_STACK_RESULT: Regex =
        Regex::new(r"✅|❌|✨").unwrap();
    static ref CDK_OUTPUT_LINE: Regex =
        Regex::new(r"^[A-Za-z0-9]+\.[A-Za-z0-9]+ = ").unwrap();
    static ref CDK_SYNTH_RESOURCE: Regex =
        Regex::new(r#""Type":\s*"AWS::"#).unwrap();
    static ref CDK_SEPARATOR: Regex =
        Regex::new(r"^[─┌┐└┘│├┤┬┴┼═╔╗╚╝║╠╣╦╩╬+\-|]+$").unwrap();
    static ref CDK_PROGRESS_LINE: Regex =
        Regex::new(r"│\s*(Creating|Deleting|Updating|UPDATE_IN_PROGRESS|CREATE_IN_PROGRESS|DELETE_IN_PROGRESS|ROLLBACK)").unwrap();
    static ref CDK_STACK_ARN: Regex =
        Regex::new(r"Stack ARN:").unwrap();
    static ref CDK_OUTPUTS_SECTION: Regex =
        Regex::new(r"^Outputs:$").unwrap();
}

/// Run a CDK command with token-optimized output.
pub fn run(subcommand: &str, args: &[String], verbose: u8) -> Result<()> {
    match subcommand {
        "synth" => run_synth(args, verbose),
        "diff" => run_diff(args, verbose),
        "deploy" => run_deploy(args, verbose),
        "destroy" => run_destroy(args, verbose),
        "ls" | "list" => run_passthrough(subcommand, args, verbose),
        _ => run_passthrough(subcommand, args, verbose),
    }
}

fn run_cdk(
    subcommand: &str,
    args: &[String],
    verbose: u8,
) -> Result<(String, String, std::process::ExitStatus)> {
    let mut cmd = Command::new("cdk");
    cmd.arg(subcommand);
    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: cdk {} {}", subcommand, args.join(" "));
    }

    let output = cmd
        .output()
        .context(format!("Failed to run cdk {}", subcommand))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        eprintln!("{}", stderr.trim());
    }

    Ok((stdout, stderr, output.status))
}

fn run_synth(args: &[String], verbose: u8) -> Result<()> {
    let timer = tracking::TimedExecution::start();
    let (raw, stderr, status) = run_cdk("synth", args, verbose)?;

    if !status.success() {
        timer.track("cdk synth", "rtk cdk synth", &stderr, &stderr);
        eprintln!("{}", stderr.trim());
        std::process::exit(status.code().unwrap_or(1));
    }

    let filtered = match filter_synth(&raw) {
        Some(f) => {
            println!("{}", f);
            f
        }
        None => {
            print!("{}", raw);
            raw.clone()
        }
    };

    timer.track("cdk synth", "rtk cdk synth", &raw, &filtered);
    Ok(())
}

fn run_diff(args: &[String], verbose: u8) -> Result<()> {
    let timer = tracking::TimedExecution::start();
    let (raw, stderr, status) = run_cdk("diff", args, verbose)?;

    // cdk diff exits 1 when differences are found (by design) and sends output
    // to stderr. Only bail on genuine failure (both stdout and stderr empty).
    if raw.is_empty() && stderr.is_empty() {
        if !status.success() {
            std::process::exit(status.code().unwrap_or(1));
        }
        println!("cdk diff: no changes");
        return Ok(());
    }

    let combined = if !raw.is_empty() { &raw } else { &stderr };

    let filtered = match filter_diff(combined) {
        Some(f) => {
            println!("{}", f);
            f
        }
        None => {
            print!("{}", combined);
            combined.clone()
        }
    };

    timer.track("cdk diff", "rtk cdk diff", combined, &filtered);

    // Propagate exit code: non-zero means differences found
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn run_deploy(args: &[String], verbose: u8) -> Result<()> {
    let timer = tracking::TimedExecution::start();
    let (raw, stderr, status) = run_cdk("deploy", args, verbose)?;

    if !status.success() {
        timer.track("cdk deploy", "rtk cdk deploy", &stderr, &stderr);
        eprintln!("{}", stderr.trim());
        std::process::exit(status.code().unwrap_or(1));
    }

    let combined = format!("{}{}", raw, stderr);
    let filtered = match filter_deploy(&combined) {
        Some(f) => {
            println!("{}", f);
            f
        }
        None => {
            print!("{}", combined);
            combined.clone()
        }
    };

    timer.track("cdk deploy", "rtk cdk deploy", &combined, &filtered);
    Ok(())
}

fn run_destroy(args: &[String], verbose: u8) -> Result<()> {
    let timer = tracking::TimedExecution::start();
    let (raw, stderr, status) = run_cdk("destroy", args, verbose)?;

    if !status.success() {
        timer.track("cdk destroy", "rtk cdk destroy", &stderr, &stderr);
        eprintln!("{}", stderr.trim());
        std::process::exit(status.code().unwrap_or(1));
    }

    let combined = format!("{}{}", raw, stderr);
    let filtered = match filter_deploy(&combined) {
        Some(f) => {
            println!("{}", f);
            f
        }
        None => {
            print!("{}", combined);
            combined.clone()
        }
    };

    timer.track("cdk destroy", "rtk cdk destroy", &combined, &filtered);
    Ok(())
}

fn run_passthrough(subcommand: &str, args: &[String], verbose: u8) -> Result<()> {
    let timer = tracking::TimedExecution::start();

    let mut cmd = Command::new("cdk");
    cmd.arg(subcommand);
    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: cdk {} {}", subcommand, args.join(" "));
    }

    let output = cmd
        .output()
        .context(format!("Failed to run cdk {}", subcommand))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let full_cmd = format!("cdk {} {}", subcommand, args.join(" "));

    print!("{}", stdout);
    if !output.status.success() {
        eprintln!("{}", stderr.trim());
    }

    timer.track_passthrough(&full_cmd, &format!("rtk {}", full_cmd));

    if !output.status.success() {
        std::process::exit(output.status.code().unwrap_or(1));
    }

    Ok(())
}

// --- Filter functions ---

/// Filter `cdk synth` output: count resources, suppress JSON template.
fn filter_synth(output: &str) -> Option<String> {
    let resource_count = CDK_SYNTH_RESOURCE.find_iter(output).count();

    if resource_count == 0 {
        return None;
    }

    // Extract warning/error lines
    let mut warnings: Vec<&str> = Vec::new();
    for line in output.lines() {
        let l = line.trim();
        if l.starts_with("[Warning]") || l.starts_with("[Error]") || l.contains("error") {
            warnings.push(l);
        }
    }

    // Try to find stack name from the template JSON
    let stack_name = output
        .lines()
        .find(|l| l.contains("TemplateURL") || l.contains("\"Description\""))
        .map(|_| "stack")
        .unwrap_or("stack");

    let mut result = format!("cdk synth: {} — {} resources", stack_name, resource_count);

    if !warnings.is_empty() {
        result.push('\n');
        for w in warnings.iter().take(5) {
            result.push_str(&format!("  {}\n", w));
        }
    }

    Some(result.trim_end().to_string())
}

/// Filter `cdk diff` output: keep resource change lines, group by construct, strip table borders.
fn filter_diff(output: &str) -> Option<String> {
    // Collect resource changes grouped by top-level construct
    let mut groups: std::collections::BTreeMap<String, Vec<(String, String, String)>> =
        std::collections::BTreeMap::new();
    let mut header_lines: Vec<String> = Vec::new();
    let mut found_changes = false;

    for line in output.lines() {
        // Skip box-drawing border lines
        if CDK_SEPARATOR.is_match(line.trim()) {
            continue;
        }

        if let Some(caps) = CDK_RESOURCE_CHANGE.captures(line) {
            found_changes = true;
            let action = caps[1].to_string();
            let resource_type = caps[2].to_string();
            let logical_id = caps[3].to_string();

            // Extract top-level construct from CDK logical ID (slash-separated path)
            let top_construct = logical_id
                .split('/')
                .next()
                .unwrap_or(&logical_id)
                .to_string();

            groups
                .entry(top_construct)
                .or_default()
                .push((action, resource_type, logical_id));
        } else if line.starts_with("Stack ") || line.contains("Resources") && !line.contains('{') {
            header_lines.push(line.trim().to_string());
        }
    }

    if !found_changes {
        // Check if there's a "no changes" message
        if output.contains("There were no differences") || output.contains("no differences") {
            return Some("cdk diff: no changes".to_string());
        }
        return None;
    }

    let mut result: Vec<String> = Vec::new();

    // Print stack header if present
    for h in &header_lines {
        if !h.is_empty() {
            result.push(h.clone());
        }
    }

    // Print grouped resource changes
    for (construct, resources) in &groups {
        let types_str: Vec<String> = resources
            .iter()
            .map(|(action, rtype, _)| format!("[{}] {}", action, rtype))
            .collect();

        if resources.len() == 1 {
            let (action, rtype, _) = &resources[0];
            result.push(format!("[{}] {} ({})", action, construct, rtype));
        } else {
            // Multiple resources under same construct
            let first = &types_str[0];
            let extra = resources.len() - 1;
            result.push(format!("{} {} +{} more", first, construct, extra));
        }
    }

    Some(result.join("\n"))
}

/// Filter `cdk deploy` / `cdk destroy` output: keep result lines and outputs.
fn filter_deploy(output: &str) -> Option<String> {
    let mut result: Vec<String> = Vec::new();
    let mut in_outputs = false;

    for line in output.lines() {
        let trimmed = line.trim();

        // Skip progress/noise lines
        if CDK_PROGRESS_LINE.is_match(line) {
            continue;
        }

        // Keep result lines (✅/❌/✨)
        if CDK_STACK_RESULT.is_match(line) {
            result.push(trimmed.to_string());
            continue;
        }

        // Keep Stack ARN line
        if CDK_STACK_ARN.is_match(line) {
            result.push(trimmed.to_string());
            continue;
        }

        // Keep Outputs section
        if CDK_OUTPUTS_SECTION.is_match(trimmed) {
            in_outputs = true;
            result.push(trimmed.to_string());
            continue;
        }

        if in_outputs {
            if trimmed.is_empty() {
                in_outputs = false;
            } else if CDK_OUTPUT_LINE.is_match(trimmed) {
                result.push(trimmed.to_string());
            }
        }
    }

    if result.is_empty() {
        None
    } else {
        Some(result.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_tokens(text: &str) -> usize {
        text.split_whitespace().count()
    }

    #[test]
    fn test_filter_synth_basic() {
        let input = r#"{
  "Resources": {
    "MyServiceTaskDef": {
      "Type": "AWS::ECS::TaskDefinition",
      "Properties": {}
    },
    "MyServiceService": {
      "Type": "AWS::ECS::Service",
      "Properties": {}
    },
    "MyApiRestApi": {
      "Type": "AWS::ApiGateway::RestApi",
      "Properties": {}
    }
  }
}"#;
        let result = filter_synth(input).unwrap();
        assert!(result.contains("3 resources"));
        assert!(!result.contains("TaskDefinition"));
        assert!(!result.contains("Properties"));
    }

    #[test]
    fn test_filter_synth_no_resources() {
        let result = filter_synth("{}");
        assert!(result.is_none());
    }

    #[test]
    fn test_filter_diff_basic() {
        let input = r#"Stack MyStack
Resources
[+] AWS::ECS::Service MyService/Service MyServiceServiceABCD1234
[-] AWS::SQS::Queue OldQueue OldQueueXYZ9876
[~] AWS::ApiGateway::Stage MyApi/Stage MyApiStageEFGH5678
"#;
        let result = filter_diff(input).unwrap();
        assert!(result.contains("[+]"));
        assert!(result.contains("MyService"));
        assert!(result.contains("[-]"));
        assert!(result.contains("OldQueue"));
    }

    #[test]
    fn test_filter_diff_no_changes() {
        let input = "There were no differences";
        let result = filter_diff(input).unwrap();
        assert_eq!(result, "cdk diff: no changes");
    }

    #[test]
    fn test_filter_diff_strips_borders() {
        let input = r#"Stack MyStack
────────────────────────────────
[+] AWS::ECS::Service MyService MyServiceABC
────────────────────────────────
"#;
        let result = filter_diff(input).unwrap();
        assert!(!result.contains("────"));
    }

    #[test]
    fn test_filter_deploy_basic() {
        let input = r#"MyStack: deploying...
 ✅  MyStack

Stack ARN:
arn:aws:cloudformation:us-east-1:123456789012:stack/MyStack/abc123

Outputs:
MyStack.ApiUrl = https://abc123.execute-api.us-east-1.amazonaws.com/prod
MyStack.BucketName = my-bucket-abc123
"#;
        let result = filter_deploy(input).unwrap();
        assert!(result.contains("✅"));
        assert!(result.contains("Stack ARN:"));
        assert!(result.contains("Outputs:"));
        assert!(result.contains("MyStack.ApiUrl"));
        assert!(!result.contains("deploying..."));
    }

    #[test]
    fn test_filter_deploy_strips_progress() {
        let input = r#"│ Creating MyBucket (AWS::S3::Bucket)
│ UPDATE_IN_PROGRESS MyStack
✅  MyStack
"#;
        let result = filter_deploy(input).unwrap();
        assert!(result.contains("✅"));
        assert!(!result.contains("Creating"));
        assert!(!result.contains("UPDATE_IN_PROGRESS"));
    }

    #[test]
    fn test_filter_deploy_empty() {
        let result = filter_deploy("some random noise\nmore noise\n");
        assert!(result.is_none());
    }

    #[test]
    fn test_synth_token_savings() {
        let input = include_str!("../tests/fixtures/cdk_synth_raw.txt");
        let result = filter_synth(input).unwrap();
        let savings = 100.0 - (count_tokens(&result) as f64 / count_tokens(input) as f64 * 100.0);
        assert!(
            savings >= 60.0,
            "CDK synth filter: expected >=60% savings, got {:.1}%",
            savings
        );
    }

    #[test]
    fn test_diff_token_savings() {
        let input = include_str!("../tests/fixtures/cdk_diff_raw.txt");
        let result = filter_diff(input).unwrap();
        let savings = 100.0 - (count_tokens(&result) as f64 / count_tokens(input) as f64 * 100.0);
        assert!(
            savings >= 60.0,
            "CDK diff filter: expected >=60% savings, got {:.1}%",
            savings
        );
    }

    #[test]
    fn test_deploy_token_savings() {
        let input = include_str!("../tests/fixtures/cdk_deploy_raw.txt");
        let result = filter_deploy(input).unwrap();
        let savings = 100.0 - (count_tokens(&result) as f64 / count_tokens(input) as f64 * 100.0);
        assert!(
            savings >= 60.0,
            "CDK deploy filter: expected >=60% savings, got {:.1}%",
            savings
        );
    }
}
