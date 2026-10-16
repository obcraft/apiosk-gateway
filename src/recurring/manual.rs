use super::*;

fn local_setting(flag: Option<&str>, bind: Option<&str>) -> bool {
    flag == Some("1")
        && bind.unwrap_or("127.0.0.1:8082")
            .parse::<std::net::SocketAddr>()
            .is_ok_and(|address| address.ip().is_loopback())
}

pub fn enabled() -> bool {
    super::enabled() && local_setting(
        std::env::var("RECURRING_MANUAL_TRIGGER_ENABLED").ok().as_deref(),
        std::env::var("BIND_ADDRESS").ok().as_deref(),
    )
}

/// Start one already approved occurrence early, preserving its identity,
/// scheduled date, funding and the normal worker's retry/delivery semantics.
pub async fn trigger(e: &Arc<Engine>, p: &Principal, order: Uuid, run: Uuid) -> anyhow::Result<()> {
    anyhow::ensure!(enabled() && enabled_for(p.owner), "Manual trigger is unavailable.");
    e.store.check_spending(p).await?;
    let lease = Uuid::new_v4();
    let scheduled = sqlx::query_scalar::<_, DateTime<Utc>>(include_str!("manual_claim.sql"))
        .bind(run).bind(order).bind(p.owner).bind(p.workspace).bind(lease)
        .fetch_optional(&e.store.pool).await?;
    let Some(scheduled) = scheduled else {
        anyhow::bail!("This occurrence is no longer available. Refresh the order.");
    };
    let e = e.clone();
    tokio::spawn(async move {
        if let Err(error) = super::worker::run_claimed(&e, run, order, scheduled, lease).await {
            // The durable lease lets the regular worker recover this exact run.
            tracing::warn!(%error, %run, "manual recurring execution failed");
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_trigger_requires_opt_in_and_loopback_binding() {
        for bind in [Some("127.0.0.1:8082"), Some("[::1]:8082"), None] {
            assert!(local_setting(Some("1"), bind));
            assert!(!local_setting(None, bind));
            assert!(!local_setting(Some("0"), bind));
        }
        for bind in ["0.0.0.0:8082", "[::]:8082", "192.168.1.2:8082", "localhost:8082", "invalid"] {
            assert!(!local_setting(Some("1"), Some(bind)));
        }
    }
}
