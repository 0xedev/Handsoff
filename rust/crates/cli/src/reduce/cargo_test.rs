pub fn reduce(output: &str) -> String {
    let mut in_failures = false;
    let mut failure_lines: Vec<&str> = Vec::new();
    let mut failure_names: Vec<&str> = Vec::new();
    let mut summary_line = "";

    for line in output.lines() {
        if line.starts_with("failures:") {
            in_failures = true;
        }
        if in_failures {
            failure_lines.push(line);
            // Cap at 200 lines total for failure details to avoid blowing up context
            if failure_lines.len() > 200 {
                failure_lines.push("... [failure details truncated]");
                break;
            }
        }
        if line.contains("... FAILED") {
            if let Some(name) = line.split("test ").nth(1) {
                failure_names.push(name.trim_end_matches(" ... FAILED"));
            }
        }
        if line.starts_with("test result:") {
            summary_line = line;
        }
    }

    if failure_names.is_empty() {
        if !summary_line.is_empty() {
            return format!("{}\n(all tests passed)", summary_line);
        } else {
            return "No tests found or output unparseable.".into();
        }
    }

    let mut out = format!("FAILED tests: {}\n\n", failure_names.join(", "));
    // Include the failure block
    for l in failure_lines {
        out.push_str(l);
        out.push('\n');
    }
    out.push_str(&format!("\n{}", summary_line));
    out
}
