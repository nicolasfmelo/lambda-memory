pub(crate) fn overlap(task: &str, text: &str) -> usize {
    fn words(text: &str) -> Vec<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_owned)
            .collect()
    }
    let mut query = words(task);
    query.sort();
    query.dedup();
    let content = words(text);
    query.iter().filter(|word| content.contains(word)).count()
}
