use holoiroh_daemon::holo_bridge::a2a_client::A2aClient;

#[tokio::main]
async fn main() {
    let client = A2aClient::new("http://192.0.2.1:1".to_string(), "probe-token".to_string());

    let Err(err) = client.probe_agent_card().await else {
        panic!("a blackholed TEST-NET-1 address must not answer an agent-card probe");
    };

    let what_the_log_used_to_show = format!("{err}");
    let what_the_log_shows_now = format!("{err:#}");

    println!("Display  ({{err}})   -> {what_the_log_used_to_show}");
    println!("Alternate ({{err:#}}) -> {what_the_log_shows_now}");

    assert!(
        what_the_log_shows_now.len() > what_the_log_used_to_show.len(),
        "REGRESSION: the alternate form carries no more information than plain Display, so the \
         underlying cause is still being dropped from the log"
    );
    assert!(
        what_the_log_shows_now.starts_with(&what_the_log_used_to_show),
        "the alternate form should extend the context, not replace it"
    );
    assert!(
        !what_the_log_used_to_show.contains(':')
            || what_the_log_shows_now.matches(':').count()
                > what_the_log_used_to_show.matches(':').count(),
        "the alternate form should expose at least one further source in the chain"
    );

    println!(
        "recovered {} extra characters of causal chain that the old log line threw away",
        what_the_log_shows_now.len() - what_the_log_used_to_show.len()
    );
    println!("VERDICT: OK -- the failure log now carries the underlying cause, not just the context");
}
