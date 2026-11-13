/// Resolves an application name against enumerated process names.
pub fn find_process_id(requested_name: &str, processes: &[(u32, String)]) -> Option<u32> {
    let requested = normalize(requested_name);

    if requested.is_empty() {
        return None;
    }

    // Prefer an exact process-name match.
    if let Some((pid, _)) = processes
        .iter()
        .find(|(_, name)| normalize(name) == requested)
    {
        return Some(*pid);
    }

    // Fall back to matching the final word of the application name.
    let final_token = requested_name
        .trim()
        .split_whitespace()
        .last()
        .map(normalize)?;

    if final_token.is_empty() {
        return None;
    }

    processes
        .iter()
        .find(|(_, name)| normalize(name) == final_token)
        .map(|(pid, _)| *pid)
}

fn normalize(name: &str) -> String {
    let normalized = name.trim().to_lowercase();

    normalized
        .strip_suffix(".exe")
        .unwrap_or(&normalized)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_exact_process_name() {
        let processes = vec![(100, "chrome".into())];
        assert_eq!(find_process_id("chrome", &processes), Some(100));
    }

    #[test]
    fn matches_case_insensitively() {
        let processes = vec![(100, "CHROME".into())];
        assert_eq!(find_process_id("chrome", &processes), Some(100));
    }

    #[test]
    fn matches_display_name_using_final_token() {
        let processes = vec![(100, "chrome".into())];
        assert_eq!(find_process_id("Google Chrome", &processes), Some(100));
    }
    #[test]
    fn prefers_exact_match_over_fallback() {
        let processes = vec![(100, "chrome".into()), (200, "googlechrome".into())];

        assert_eq!(find_process_id("chrome", &processes), Some(100));
    }

    #[test]
    fn does_not_match_partial_process_names() {
        let processes = vec![(100, "chromium".into())];

        assert_eq!(find_process_id("chrome", &processes), None);
    }

    #[test]
    fn handles_whitespace() {
        let processes = vec![(100, "chrome".into())];
        assert_eq!(find_process_id("  Google Chrome  ", &processes), Some(100));
    }

    #[test]
    fn handles_exe_extension() {
        let processes = vec![(100, "chrome.exe".into())];
        assert_eq!(find_process_id("chrome", &processes), Some(100));
    }

    #[test]
    fn rejects_empty_names() {
        assert_eq!(find_process_id("  ", &[]), None);
    }

    #[test]
    fn returns_none_for_unmatched_names() {
        let processes = vec![(100, "chrome".into())];
        assert_eq!(find_process_id("Firefox", &processes), None);
    }
}
