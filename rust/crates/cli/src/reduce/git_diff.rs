pub fn reduce(output: &str) -> String {
    let mut result = String::new();
    let mut hunk_lines = 0usize;
    let mut in_hunk = false;

    for line in output.lines() {
        if line.starts_with("diff --git") {
            result.push_str(line);
            result.push('\n');
            hunk_lines = 0;
            in_hunk = false;
        } else if line.starts_with("@@") {
            in_hunk = true;
            hunk_lines = 0;
            result.push_str(line);
            result.push('\n');
        } else if in_hunk {
            hunk_lines += 1;
            if hunk_lines <= 100 {
                result.push_str(line);
                result.push('\n');
            } else if hunk_lines == 101 {
                result.push_str("... [hunk truncated — too large]\n");
            }
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}
