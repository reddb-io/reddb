//! Ephemeral notification primitive (issue #720, PRD #718).
//!
//! Tenant-scoped pub/sub signals with no replay, ACK, consumer
//! offset, pending delivery, or DLQ. Offline listeners miss
//! notifications by design — applications that need durability
//! should use queues or streams instead. ADR 0028 pins this
//! boundary: queue wait, notification, and stream are separate
//! primitives because their state machines are incompatible.
//!
//! ## Contract surface
//!
//! - `NotificationRegistry::publish_authorized` — capability-gated
//!   publish that records whether the principal is allowed to
//!   target the requested scope. Same-tenant publishes succeed
//!   without an explicit capability; cross-tenant or global
//!   publishes require the caller to assert
//!   `has_cross_tenant_cap`, which is supplied by the calling
//!   transport after evaluating the `notify:cross-tenant` action
//!   against the principal's effective policies.
//! - `NotificationRegistry::subscribe_authorized` — same capability
//!   gate, but for the read side: subscribing to another tenant's
//!   channel (or to the global namespace) requires the cross-tenant
//!   capability.
//! - `NotificationRegistry::publish` /
//!   `NotificationRegistry::subscribe` — unauthenticated entry
//!   points used by tests and by callers that have already proven
//!   they sit above the authorization boundary. Transports should
//!   prefer the `_authorized` variants.
//!
//! ## No-replay semantics
//!
//! The registry stores one Tokio broadcast channel per
//! `(scope, channel)` key. A late subscriber's
//! `broadcast::Sender::subscribe` cursor starts at the channel's
//! current tail, so notifications published before the subscriber
//! connected are not delivered — that is the no-replay guarantee.
//! Offline listeners that reconnect therefore start with an empty
//! queue and observe only future notifications, which is the
//! deliberate trade-off: ephemeral channels do not buffer for
//! disconnected consumers. Channels with no active receivers
//! drop the underlying sender, so memory cost is bounded by the
//! number of *connected* listeners.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::broadcast;

/// Scope of a notification channel.
///
/// `Tenant(id)` is the default and matches RedDB's tenancy model —
/// channels live inside a tenant and are invisible to other
/// tenants. `Global` is the cross-tenant / platform namespace and
/// requires the `notify:cross-tenant` capability to address.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NotificationScope {
    /// Tenant-scoped channel.
    Tenant(String),
    /// Cross-tenant / platform-global channel.
    Global,
}

impl NotificationScope {
    /// Construct a scope from a principal's tenant binding.
    ///
    /// `Some("acme")` becomes `Tenant("acme")`; `None` becomes
    /// `Global` (the platform tenant — matches
    /// `auth::UserId::platform`).
    pub fn from_principal_tenant(tenant: Option<&str>) -> Self {
        match tenant {
            Some(t) => NotificationScope::Tenant(t.to_string()),
            None => NotificationScope::Global,
        }
    }

    /// Stable string identifier used in audit events.
    pub fn label(&self) -> String {
        match self {
            NotificationScope::Tenant(t) => format!("tenant:{t}"),
            NotificationScope::Global => "global".to_string(),
        }
    }
}

/// A single notification delivered to one connected listener.
///
/// Carries the routing tuple (scope + channel) plus an opaque
/// UTF-8 payload. `published_at_ms` is monotonically meaningful
/// only within the running process; offline-replay use cases
/// should be modelled as queues or streams, not notifications.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationEvent {
    pub scope: NotificationScope,
    pub channel: String,
    pub payload: String,
    pub published_at_ms: u128,
}

/// Errors returned by the notification authorization gate.
#[derive(Debug, PartialEq, Eq)]
pub enum NotificationError {
    /// The principal tried to address a channel outside their own
    /// tenant without the `notify:cross-tenant` capability. The
    /// caller knows the principal's tenant and the requested
    /// scope; the error preserves both so audit can reconstruct
    /// the denial.
    CrossTenantDenied {
        principal_tenant: Option<String>,
        target: NotificationScope,
        channel: String,
    },
}

impl std::fmt::Display for NotificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotificationError::CrossTenantDenied {
                principal_tenant,
                target,
                channel,
            } => {
                let from = principal_tenant.as_deref().unwrap_or("<platform>");
                write!(
                    f,
                    "notification: principal in tenant `{}` is not allowed to address `{}` channel `{}` without the `notify:cross-tenant` capability",
                    from,
                    target.label(),
                    channel
                )
            }
        }
    }
}

impl std::error::Error for NotificationError {}

/// Per-channel broadcast capacity. Tuned for low-fanout signals;
/// callers that need higher throughput should batch into a single
/// notification or graduate to a durable stream.
const CHANNEL_CAPACITY: usize = 256;

/// In-memory registry of ephemeral notification channels.
///
/// The registry is `Send + Sync` and intended to live behind an
/// `Arc` on the runtime — typically one per server process. It
/// owns no on-disk state, no WAL entries, and no audit-event tail:
/// a process restart drops every channel and every connected
/// listener, which is the intended ephemeral contract.
#[derive(Default, Clone)]
pub struct NotificationRegistry {
    inner: Arc<Mutex<HashMap<ChannelKey, broadcast::Sender<NotificationEvent>>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ChannelKey {
    scope: NotificationScope,
    channel: String,
}

impl NotificationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe to `(scope, channel)`.
    ///
    /// The returned receiver only observes notifications published
    /// AFTER `subscribe` returns. Drop the receiver to unsubscribe;
    /// the underlying channel is reaped from the registry when its
    /// last receiver is dropped and no senders remain outstanding.
    pub fn subscribe(
        &self,
        scope: NotificationScope,
        channel: impl Into<String>,
    ) -> broadcast::Receiver<NotificationEvent> {
        let key = ChannelKey {
            scope,
            channel: channel.into(),
        };
        let mut guard = self.inner.lock();
        let sender = guard
            .entry(key)
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0);
        sender.subscribe()
    }

    /// Publish a payload on `(scope, channel)` and return the
    /// number of currently connected listeners that received the
    /// event. Returns `0` if no listeners are connected — the
    /// notification is dropped (no buffering, no replay).
    pub fn publish(
        &self,
        scope: NotificationScope,
        channel: impl Into<String>,
        payload: impl Into<String>,
        now_ms: u128,
    ) -> usize {
        let channel = channel.into();
        let key = ChannelKey {
            scope: scope.clone(),
            channel: channel.clone(),
        };
        let event = NotificationEvent {
            scope,
            channel,
            payload: payload.into(),
            published_at_ms: now_ms,
        };

        let sender = {
            let guard = self.inner.lock();
            guard.get(&key).cloned()
        };
        let Some(sender) = sender else {
            return 0;
        };

        // If no live receivers, drop the event and reap the
        // channel so future publishes on a dead channel don't
        // accumulate buffered messages — ephemeral semantics.
        if sender.receiver_count() == 0 {
            self.inner.lock().remove(&key);
            return 0;
        }
        sender.send(event).unwrap_or(0)
    }

    /// Authorization-gated publish.
    ///
    /// `principal_tenant` is the publisher's tenant binding
    /// (`None` = platform/global). `target` is the channel scope
    /// the publisher asked for. `has_cross_tenant_cap` must be
    /// `true` if the calling transport's policy evaluator
    /// previously granted the principal the `notify:cross-tenant`
    /// action; the registry does not consult policies directly,
    /// keeping the authorization boundary on the transport side
    /// (mirrors the AiProviderGate pattern from #711).
    pub fn publish_authorized(
        &self,
        principal_tenant: Option<&str>,
        target: NotificationScope,
        channel: impl Into<String>,
        payload: impl Into<String>,
        has_cross_tenant_cap: bool,
        now_ms: u128,
    ) -> Result<usize, NotificationError> {
        let channel = channel.into();
        Self::authorize(principal_tenant, &target, &channel, has_cross_tenant_cap)?;
        Ok(self.publish(target, channel, payload, now_ms))
    }

    /// Authorization-gated subscribe — mirror of
    /// [`Self::publish_authorized`] for the read side.
    pub fn subscribe_authorized(
        &self,
        principal_tenant: Option<&str>,
        target: NotificationScope,
        channel: impl Into<String>,
        has_cross_tenant_cap: bool,
    ) -> Result<broadcast::Receiver<NotificationEvent>, NotificationError> {
        let channel = channel.into();
        Self::authorize(principal_tenant, &target, &channel, has_cross_tenant_cap)?;
        Ok(self.subscribe(target, channel))
    }

    fn authorize(
        principal_tenant: Option<&str>,
        target: &NotificationScope,
        channel: &str,
        has_cross_tenant_cap: bool,
    ) -> Result<(), NotificationError> {
        let same_scope = match (principal_tenant, target) {
            (Some(pt), NotificationScope::Tenant(tt)) => pt == tt,
            // A platform/system principal (tenant=None) addressing
            // the Global namespace is operating inside its own scope
            // and needs no extra capability.
            (None, NotificationScope::Global) => true,
            _ => false,
        };
        if same_scope || has_cross_tenant_cap {
            return Ok(());
        }
        Err(NotificationError::CrossTenantDenied {
            principal_tenant: principal_tenant.map(str::to_string),
            target: target.clone(),
            channel: channel.to_string(),
        })
    }

    /// Number of channels currently registered. Test/diagnostic
    /// helper — operators should not depend on this value.
    pub fn channel_count(&self) -> usize {
        self.inner.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> u128 {
        // Deterministic-enough counter for tests; the actual value
        // isn't asserted on, only that publishes carry *some*
        // timestamp.
        1
    }

    #[test]
    fn same_tenant_publish_subscribe_round_trip() {
        let reg = NotificationRegistry::new();
        let mut rx = reg.subscribe(NotificationScope::Tenant("acme".into()), "deploys");
        let delivered = reg.publish(
            NotificationScope::Tenant("acme".into()),
            "deploys",
            "v1.2.3",
            now(),
        );
        assert_eq!(delivered, 1, "one connected listener should receive");
        let event = rx.try_recv().expect("event delivered");
        assert_eq!(event.channel, "deploys");
        assert_eq!(event.payload, "v1.2.3");
        assert_eq!(event.scope, NotificationScope::Tenant("acme".into()));
    }

    #[test]
    fn channels_are_tenant_isolated() {
        let reg = NotificationRegistry::new();
        let mut rx_acme = reg.subscribe(NotificationScope::Tenant("acme".into()), "deploys");
        let mut rx_globex = reg.subscribe(NotificationScope::Tenant("globex".into()), "deploys");

        // Publish to acme only — globex must not see it. Same
        // channel name, different tenant scope.
        reg.publish(
            NotificationScope::Tenant("acme".into()),
            "deploys",
            "acme-only",
            now(),
        );

        assert_eq!(rx_acme.try_recv().unwrap().payload, "acme-only");
        assert!(
            rx_globex.try_recv().is_err(),
            "globex must not see acme's notification"
        );
    }

    #[test]
    fn channel_names_are_scoped_independently() {
        let reg = NotificationRegistry::new();
        let mut rx_a = reg.subscribe(NotificationScope::Tenant("acme".into()), "a");
        let mut rx_b = reg.subscribe(NotificationScope::Tenant("acme".into()), "b");

        reg.publish(NotificationScope::Tenant("acme".into()), "a", "to-a", now());

        assert_eq!(rx_a.try_recv().unwrap().payload, "to-a");
        assert!(rx_b.try_recv().is_err());
    }

    #[test]
    fn offline_listeners_miss_notifications_no_replay() {
        let reg = NotificationRegistry::new();

        // Phase 1: a subscriber connects, then disconnects.
        {
            let _rx = reg.subscribe(NotificationScope::Tenant("acme".into()), "deploys");
        }

        // Phase 2: publish while no one is listening — dropped.
        let delivered = reg.publish(
            NotificationScope::Tenant("acme".into()),
            "deploys",
            "v1.0.0",
            now(),
        );
        assert_eq!(delivered, 0, "publish with no listeners delivers 0");

        // Phase 3: subscriber reconnects — must NOT see the
        // pre-reconnect notification.
        let mut rx = reg.subscribe(NotificationScope::Tenant("acme".into()), "deploys");
        assert!(
            rx.try_recv().is_err(),
            "reconnected listener must not receive pre-reconnect notifications",
        );

        // Phase 4: publish after reconnect — listener gets the
        // new notification.
        reg.publish(
            NotificationScope::Tenant("acme".into()),
            "deploys",
            "v2.0.0",
            now(),
        );
        assert_eq!(rx.try_recv().unwrap().payload, "v2.0.0");
    }

    #[test]
    fn fanout_to_all_connected_listeners() {
        let reg = NotificationRegistry::new();
        let mut rx1 = reg.subscribe(NotificationScope::Tenant("acme".into()), "deploys");
        let mut rx2 = reg.subscribe(NotificationScope::Tenant("acme".into()), "deploys");
        let mut rx3 = reg.subscribe(NotificationScope::Tenant("acme".into()), "deploys");

        let delivered = reg.publish(
            NotificationScope::Tenant("acme".into()),
            "deploys",
            "fanout",
            now(),
        );
        assert_eq!(delivered, 3);
        for rx in [&mut rx1, &mut rx2, &mut rx3] {
            assert_eq!(rx.try_recv().unwrap().payload, "fanout");
        }
    }

    #[test]
    fn same_tenant_publish_does_not_require_cross_tenant_cap() {
        let reg = NotificationRegistry::new();
        let mut rx = reg
            .subscribe_authorized(
                Some("acme"),
                NotificationScope::Tenant("acme".into()),
                "deploys",
                false, // no cross-tenant cap
            )
            .expect("same-tenant subscribe must succeed without cross-tenant cap");

        let delivered = reg
            .publish_authorized(
                Some("acme"),
                NotificationScope::Tenant("acme".into()),
                "deploys",
                "v1",
                false,
                now(),
            )
            .expect("same-tenant publish must succeed without cross-tenant cap");
        assert_eq!(delivered, 1);
        assert_eq!(rx.try_recv().unwrap().payload, "v1");
    }

    #[test]
    fn cross_tenant_publish_denied_without_cap() {
        let reg = NotificationRegistry::new();
        let err = reg
            .publish_authorized(
                Some("acme"),
                NotificationScope::Tenant("globex".into()),
                "deploys",
                "leak",
                false,
                now(),
            )
            .expect_err("cross-tenant publish must be denied without cap");
        match err {
            NotificationError::CrossTenantDenied {
                principal_tenant,
                target,
                channel,
            } => {
                assert_eq!(principal_tenant.as_deref(), Some("acme"));
                assert_eq!(target, NotificationScope::Tenant("globex".into()));
                assert_eq!(channel, "deploys");
            }
        }
    }

    #[test]
    fn cross_tenant_subscribe_denied_without_cap() {
        let reg = NotificationRegistry::new();
        let err = reg
            .subscribe_authorized(
                Some("acme"),
                NotificationScope::Tenant("globex".into()),
                "deploys",
                false,
            )
            .expect_err("cross-tenant subscribe must be denied without cap");
        assert!(matches!(err, NotificationError::CrossTenantDenied { .. }));
    }

    #[test]
    fn cross_tenant_publish_allowed_with_cap() {
        let reg = NotificationRegistry::new();
        let mut rx = reg.subscribe(NotificationScope::Tenant("globex".into()), "deploys");
        let delivered = reg
            .publish_authorized(
                Some("acme"),
                NotificationScope::Tenant("globex".into()),
                "deploys",
                "allowed",
                true,
                now(),
            )
            .expect("publish with cross-tenant cap must succeed");
        assert_eq!(delivered, 1);
        assert_eq!(rx.try_recv().unwrap().payload, "allowed");
    }

    #[test]
    fn global_scope_requires_cross_tenant_cap() {
        let reg = NotificationRegistry::new();
        let err = reg
            .publish_authorized(
                Some("acme"),
                NotificationScope::Global,
                "platform",
                "leak",
                false,
                now(),
            )
            .expect_err("targeting Global from a tenant must require cap");
        assert!(matches!(err, NotificationError::CrossTenantDenied { .. }));

        // Platform principals (tenant=None) addressing Global is
        // same-scope and requires no extra cap.
        let _ = reg
            .publish_authorized(
                None,
                NotificationScope::Global,
                "platform",
                "ok",
                false,
                now(),
            )
            .expect("platform principal targeting global is same-scope");
    }

    #[test]
    fn channel_is_reaped_when_last_receiver_drops() {
        let reg = NotificationRegistry::new();
        {
            let _rx = reg.subscribe(NotificationScope::Tenant("acme".into()), "deploys");
            assert_eq!(reg.channel_count(), 1);
        }
        // Receiver dropped. The channel record itself is reaped
        // on the next publish to that key.
        reg.publish(
            NotificationScope::Tenant("acme".into()),
            "deploys",
            "noop",
            now(),
        );
        assert_eq!(reg.channel_count(), 0);
    }

    #[test]
    fn from_principal_tenant_maps_correctly() {
        assert_eq!(
            NotificationScope::from_principal_tenant(Some("acme")),
            NotificationScope::Tenant("acme".into())
        );
        assert_eq!(
            NotificationScope::from_principal_tenant(None),
            NotificationScope::Global
        );
    }
}
