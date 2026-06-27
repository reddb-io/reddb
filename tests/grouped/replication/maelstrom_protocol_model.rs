use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

const NODE_COUNT: usize = 3;
const QUORUM: usize = 2;

#[test]
fn maelstrom_protocol_model_reports_replayable_safety_runs() {
    let report = run_maelstrom_protocol_model(0x5eed).expect("seed 0x5eed stays safe");

    assert_eq!(report.trace_id, "seed=0x0000000000005eed");
    assert!(report.elections > 0, "{report:?}");
    assert!(report.client_writes > 0, "{report:?}");
    assert!(report.partitions > 0, "{report:?}");
    assert!(report.drops > 0, "{report:?}");
    assert!(report.delays > 0, "{report:?}");
    assert!(report.reorders > 0, "{report:?}");
    assert!(report.crashes > 0, "{report:?}");
}

#[test]
fn maelstrom_protocol_model_failure_reports_replay_seed_and_trace() {
    let mut harness = MaelstromHarness::new(0xbad);
    harness.nodes[0].role = Role::Leader;
    harness.nodes[0].term = 4;
    harness.nodes[1].role = Role::Leader;
    harness.nodes[1].term = 4;

    let err = harness
        .assert_invariants("injected".to_string())
        .unwrap_err();
    let rendered = err.to_string();

    assert!(rendered.contains("seed=0x0000000000000bad"), "{rendered}");
    assert!(rendered.contains("trace="), "{rendered}");
    assert!(rendered.contains("single-writer"), "{rendered}");
}

fn run_maelstrom_protocol_model(seed: u64) -> Result<ModelReport, ModelError> {
    let mut harness = MaelstromHarness::new(seed);
    harness.run(240)
}

#[derive(Debug)]
struct ModelReport {
    trace_id: String,
    elections: usize,
    client_writes: usize,
    partitions: usize,
    drops: usize,
    delays: usize,
    reorders: usize,
    crashes: usize,
}

struct MaelstromHarness {
    seed: u64,
    step: usize,
    rng: XorShift64,
    nodes: [Node; NODE_COUNT],
    links: [[bool; NODE_COUNT]; NODE_COUNT],
    messages: Vec<Message>,
    accepted_writes: Vec<AcceptedWrite>,
    committed: BTreeMap<usize, LogEntry>,
    highest_elected_term: u64,
    next_write_id: u64,
    stats: Stats,
    trace: Vec<String>,
}

impl MaelstromHarness {
    fn new(seed: u64) -> Self {
        Self {
            seed,
            step: 0,
            rng: XorShift64::new(seed),
            nodes: [Node::new(), Node::new(), Node::new()],
            links: [[true; NODE_COUNT]; NODE_COUNT],
            messages: Vec::new(),
            accepted_writes: Vec::new(),
            committed: BTreeMap::new(),
            highest_elected_term: 0,
            next_write_id: 1,
            stats: Stats::default(),
            trace: Vec::new(),
        }
    }

    fn run(&mut self, steps: usize) -> Result<ModelReport, ModelError> {
        self.bootstrap_leader_and_committed_write()?;
        for step in 0..steps {
            self.step = step;
            let action = self.next_action(step);
            self.trace.push(action.clone());
            self.apply_action(step, action)?;
            self.assert_invariants(format!("after step {step}"))?;
        }

        Ok(ModelReport {
            trace_id: self.trace_id(),
            elections: self.stats.elections,
            client_writes: self.stats.client_writes,
            partitions: self.stats.partitions,
            drops: self.stats.drops,
            delays: self.stats.delays,
            reorders: self.stats.reorders,
            crashes: self.stats.crashes,
        })
    }

    fn bootstrap_leader_and_committed_write(&mut self) -> Result<(), ModelError> {
        self.step = 0;
        self.trace.push("bootstrap timeout n0".to_string());
        self.timeout(0)?;

        for _ in 0..3 {
            self.trace.push("bootstrap deliver".to_string());
            self.deliver_one(0)?;
        }

        self.trace.push("bootstrap client-write n0".to_string());
        self.client_write(0)?;
        for _ in 0..4 {
            self.trace.push("bootstrap deliver append".to_string());
            self.deliver_one(0)?;
        }

        self.trace.push("bootstrap heartbeat reorder".to_string());
        self.enqueue_append_heartbeats(0);
        self.reorder()?;
        self.trace.push("bootstrap heartbeat delay".to_string());
        self.delay_one()?;
        self.assert_invariants("after bootstrap".to_string())
    }

    fn next_action(&mut self, step: usize) -> String {
        match step % 12 {
            0 => format!("timeout n{}", self.rng.node()),
            1 | 4 => "deliver".to_string(),
            2 => format!("client-write n{}", self.leader_or_random_node()),
            3 => format!("partition isolate n{}", self.rng.node()),
            5 => "drop".to_string(),
            6 => "heal".to_string(),
            7 => format!("crash n{}", self.rng.node()),
            8 => format!("restart n{}", self.rng.node()),
            9 => "reorder".to_string(),
            10 => "delay".to_string(),
            11 => "deliver".to_string(),
            _ => unreachable!(),
        }
    }

    fn apply_action(&mut self, step: usize, action: String) -> Result<(), ModelError> {
        let mut parts = action.split_whitespace();
        match parts.next() {
            Some("timeout") => self.timeout(parse_node(parts.next())),
            Some("deliver") => self.deliver_one(step),
            Some("client-write") => self.client_write(parse_node(parts.next())),
            Some("partition") => self.partition(parse_node(parts.nth(1))),
            Some("drop") => self.drop_one(),
            Some("heal") => self.heal(),
            Some("crash") => self.crash(parse_node(parts.next())),
            Some("restart") => self.restart(parse_node(parts.next())),
            Some("reorder") => self.reorder(),
            Some("delay") => self.delay_one(),
            _ => Ok(()),
        }
    }

    fn leader_or_random_node(&mut self) -> usize {
        self.current_leader().unwrap_or_else(|| self.rng.node())
    }

    fn current_leader(&self) -> Option<usize> {
        self.nodes
            .iter()
            .enumerate()
            .find(|(_, node)| node.alive && node.role == Role::Leader)
            .map(|(node_id, _)| node_id)
    }

    fn enqueue_append_heartbeats(&mut self, leader_id: usize) {
        let leader = &self.nodes[leader_id];
        if !leader.alive || leader.role != Role::Leader {
            return;
        }

        let term = leader.term;
        let entries = leader.log.clone();
        let leader_commit = leader.commit_index;
        for to in 0..NODE_COUNT {
            if to == leader_id {
                continue;
            }
            self.messages.push(Message {
                from: leader_id,
                to,
                deliver_at: self.step,
                kind: MessageKind::AppendEntries {
                    term,
                    entries: entries.clone(),
                    leader_commit,
                },
            });
        }
    }

    fn timeout(&mut self, node_id: usize) -> Result<(), ModelError> {
        let node = &mut self.nodes[node_id];
        if !node.alive {
            return Ok(());
        }

        node.term += 1;
        node.voted_for = Some(node_id);
        node.role = Role::Candidate;
        node.votes.clear();
        node.votes.insert(node_id);
        self.stats.elections += 1;

        for to in 0..NODE_COUNT {
            if to == node_id {
                continue;
            }
            self.messages.push(Message {
                from: node_id,
                to,
                deliver_at: self.step,
                kind: MessageKind::VoteRequest {
                    term: node.term,
                    candidate: node_id,
                    last_log_index: node.log.len(),
                    last_log_term: node.last_log_term(),
                },
            });
        }

        self.promote_if_quorum(node_id)?;
        Ok(())
    }

    fn deliver_one(&mut self, step: usize) -> Result<(), ModelError> {
        if let Some(index) = self
            .messages
            .iter()
            .position(|message| message.deliver_at <= step)
        {
            let message = self.messages.remove(index);
            if !self.links[message.from][message.to] || !self.nodes[message.to].alive {
                return Ok(());
            }
            self.handle_message(message)?;
        }
        Ok(())
    }

    fn handle_message(&mut self, message: Message) -> Result<(), ModelError> {
        match message.kind {
            MessageKind::VoteRequest {
                term,
                candidate,
                last_log_index,
                last_log_term,
            } => {
                let granted =
                    self.consider_vote(message.to, candidate, term, last_log_index, last_log_term);
                self.messages.push(Message {
                    from: message.to,
                    to: message.from,
                    deliver_at: self.step,
                    kind: MessageKind::VoteResponse { term, granted },
                });
            }
            MessageKind::VoteResponse { term, granted } => {
                let node = &mut self.nodes[message.to];
                if term > node.term {
                    node.step_down(term);
                    return Ok(());
                }
                if granted && node.alive && node.role == Role::Candidate && node.term == term {
                    node.votes.insert(message.from);
                    self.promote_if_quorum(message.to)?;
                }
            }
            MessageKind::AppendEntries {
                term,
                entries,
                leader_commit,
            } => {
                let success = self.append_entries(message.to, term, &entries, leader_commit);
                self.messages.push(Message {
                    from: message.to,
                    to: message.from,
                    deliver_at: self.step,
                    kind: MessageKind::AppendResponse { term, success },
                });
            }
            MessageKind::AppendResponse { term, success } => {
                if !success {
                    return Ok(());
                }
                if let Some(leader) = self.nodes.get_mut(message.to) {
                    if leader.role == Role::Leader && leader.term == term {
                        leader.acks.insert(message.from);
                    }
                }
                self.advance_commit(message.to)?;
            }
        }
        Ok(())
    }

    fn consider_vote(
        &mut self,
        voter_id: usize,
        candidate: usize,
        term: u64,
        last_log_index: usize,
        last_log_term: u64,
    ) -> bool {
        let voter = &mut self.nodes[voter_id];
        if !voter.alive || term < voter.term {
            return false;
        }
        if term > voter.term {
            voter.step_down(term);
        }
        if !voter.can_vote_for(candidate) {
            return false;
        }
        if !up_to_date(last_log_index, last_log_term, &voter.log) {
            return false;
        }
        voter.voted_for = Some(candidate);
        true
    }

    fn promote_if_quorum(&mut self, node_id: usize) -> Result<(), ModelError> {
        let node = &mut self.nodes[node_id];
        if node.role == Role::Candidate && node.votes.len() >= QUORUM {
            node.role = Role::Leader;
            node.acks.clear();
            node.acks.insert(node_id);
            self.highest_elected_term = self.highest_elected_term.max(node.term);
        }
        self.assert_invariants(format!("promote n{node_id}"))
    }

    fn client_write(&mut self, node_id: usize) -> Result<(), ModelError> {
        let node = &self.nodes[node_id];
        if !node.alive || node.role != Role::Leader {
            return Ok(());
        }
        if node.term < self.highest_elected_term || !self.can_reach_quorum(node_id) {
            return Ok(());
        }
        let term = node.term;

        let entry = LogEntry {
            id: self.next_write_id,
            term,
        };
        self.next_write_id += 1;

        let node = &mut self.nodes[node_id];
        node.log.push(entry);
        node.acks.clear();
        node.acks.insert(node_id);
        self.accepted_writes.push(AcceptedWrite {
            leader: node_id,
            term,
            id: entry.id,
            highest_elected_term_at_accept: self.highest_elected_term,
        });
        self.stats.client_writes += 1;

        for to in 0..NODE_COUNT {
            if to == node_id {
                continue;
            }
            self.messages.push(Message {
                from: node_id,
                to,
                deliver_at: self.step,
                kind: MessageKind::AppendEntries {
                    term,
                    entries: node.log.clone(),
                    leader_commit: node.commit_index,
                },
            });
        }
        Ok(())
    }

    fn append_entries(
        &mut self,
        follower_id: usize,
        term: u64,
        entries: &[LogEntry],
        leader_commit: usize,
    ) -> bool {
        let follower = &mut self.nodes[follower_id];
        if !follower.alive || term < follower.term {
            return false;
        }
        if term > follower.term || follower.role != Role::Follower {
            follower.step_down(term);
        }

        let shared = follower.log.len().min(entries.len());
        for index in 0..shared {
            if follower.log[index] != entries[index] {
                follower.log.truncate(index);
                break;
            }
        }
        if follower.log.len() < entries.len() {
            follower
                .log
                .extend_from_slice(&entries[follower.log.len()..]);
        }
        follower.commit_index = follower
            .commit_index
            .max(leader_commit.min(follower.log.len()));
        true
    }

    fn advance_commit(&mut self, leader_id: usize) -> Result<(), ModelError> {
        let leader = &mut self.nodes[leader_id];
        if leader.role != Role::Leader || leader.acks.len() < QUORUM || leader.log.is_empty() {
            return Ok(());
        }
        leader.commit_index = leader.log.len();
        for (index, entry) in leader.log.iter().copied().enumerate() {
            let committed_index = index + 1;
            if let Some(existing) = self.committed.insert(committed_index, entry) {
                if existing != entry {
                    return Err(self.error(
                        "committed-write-loss",
                        format!("index {committed_index} changed from {existing:?} to {entry:?}"),
                    ));
                }
            }
        }
        Ok(())
    }

    fn partition(&mut self, isolated: usize) -> Result<(), ModelError> {
        self.links = [[true; NODE_COUNT]; NODE_COUNT];
        for peer in 0..NODE_COUNT {
            if peer != isolated {
                self.links[isolated][peer] = false;
                self.links[peer][isolated] = false;
            }
        }
        self.stats.partitions += 1;
        self.assert_invariants(format!("partition n{isolated}"))
    }

    fn drop_one(&mut self) -> Result<(), ModelError> {
        if !self.messages.is_empty() {
            let index = self.rng.index(self.messages.len());
            self.messages.remove(index);
            self.stats.drops += 1;
        }
        Ok(())
    }

    fn heal(&mut self) -> Result<(), ModelError> {
        self.links = [[true; NODE_COUNT]; NODE_COUNT];
        self.assert_invariants("heal".to_string())
    }

    fn crash(&mut self, node_id: usize) -> Result<(), ModelError> {
        let node = &mut self.nodes[node_id];
        if node.alive {
            node.alive = false;
            node.role = Role::Follower;
            node.votes.clear();
            node.acks.clear();
            self.stats.crashes += 1;
        }
        self.assert_invariants(format!("crash n{node_id}"))
    }

    fn restart(&mut self, node_id: usize) -> Result<(), ModelError> {
        let node = &mut self.nodes[node_id];
        node.alive = true;
        node.role = Role::Follower;
        node.votes.clear();
        node.acks.clear();
        self.assert_invariants(format!("restart n{node_id}"))
    }

    fn reorder(&mut self) -> Result<(), ModelError> {
        if self.messages.len() < 2 {
            if let Some(leader_id) = self.current_leader() {
                self.enqueue_append_heartbeats(leader_id);
            }
        }
        if self.messages.len() >= 2 {
            let a = self.rng.index(self.messages.len());
            let mut b = self.rng.index(self.messages.len());
            if a == b {
                b = (b + 1) % self.messages.len();
            }
            self.messages.swap(a, b);
            self.stats.reorders += 1;
        }
        Ok(())
    }

    fn delay_one(&mut self) -> Result<(), ModelError> {
        if self.messages.is_empty() {
            if let Some(leader_id) = self.current_leader() {
                self.enqueue_append_heartbeats(leader_id);
            }
        }
        if !self.messages.is_empty() {
            let index = self.rng.index(self.messages.len());
            self.messages[index].deliver_at += 2;
            self.stats.delays += 1;
        }
        Ok(())
    }

    fn assert_invariants(&self, context: String) -> Result<(), ModelError> {
        let mut leaders_by_term: BTreeMap<u64, usize> = BTreeMap::new();
        for (node_id, node) in self.nodes.iter().enumerate() {
            if node.role != Role::Leader {
                continue;
            }
            if let Some(previous) = leaders_by_term.insert(node.term, node_id) {
                return Err(self.error(
                    "single-writer",
                    format!(
                        "{context}: n{previous} and n{node_id} are both leaders in term {}",
                        node.term
                    ),
                ));
            }
        }

        for write in &self.accepted_writes {
            if write.term < write.highest_elected_term_at_accept {
                return Err(self.error(
                    "stale-leader-write",
                    format!(
                        "{context}: n{} accepted write {} in stale term {} after term {} was elected",
                        write.leader, write.id, write.term, write.highest_elected_term_at_accept
                    ),
                ));
            }
        }

        for (committed_index, committed_entry) in &self.committed {
            for (node_id, node) in self.nodes.iter().enumerate() {
                if node.role != Role::Leader || node.term < committed_entry.term {
                    continue;
                }
                let entry = node.log.get(committed_index - 1);
                if entry != Some(committed_entry) {
                    return Err(self.error(
                        "committed-write-loss",
                        format!(
                            "{context}: leader n{node_id} term {} lacks committed {committed_entry:?} at index {committed_index}",
                            node.term
                        ),
                    ));
                }
            }
        }

        Ok(())
    }

    fn can_reach_quorum(&self, node_id: usize) -> bool {
        let reachable = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(peer, node)| node.alive && self.links[node_id][*peer])
            .count();
        reachable >= QUORUM
    }

    fn trace_id(&self) -> String {
        format!("seed=0x{:016x}", self.seed)
    }

    fn error(&self, property: &'static str, detail: String) -> ModelError {
        ModelError {
            trace_id: self.trace_id(),
            step: self.step,
            property,
            detail,
            trace: self.trace.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LogEntry {
    id: u64,
    term: u64,
}

#[derive(Clone, Debug)]
struct Message {
    from: usize,
    to: usize,
    deliver_at: usize,
    kind: MessageKind,
}

#[derive(Clone, Debug)]
enum MessageKind {
    VoteRequest {
        term: u64,
        candidate: usize,
        last_log_index: usize,
        last_log_term: u64,
    },
    VoteResponse {
        term: u64,
        granted: bool,
    },
    AppendEntries {
        term: u64,
        entries: Vec<LogEntry>,
        leader_commit: usize,
    },
    AppendResponse {
        term: u64,
        success: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Role {
    Follower,
    Candidate,
    Leader,
}

#[derive(Clone)]
struct Node {
    alive: bool,
    term: u64,
    voted_for: Option<usize>,
    role: Role,
    log: Vec<LogEntry>,
    commit_index: usize,
    votes: BTreeSet<usize>,
    acks: BTreeSet<usize>,
}

impl Node {
    fn new() -> Self {
        Self {
            alive: true,
            term: 0,
            voted_for: None,
            role: Role::Follower,
            log: Vec::new(),
            commit_index: 0,
            votes: BTreeSet::new(),
            acks: BTreeSet::new(),
        }
    }

    fn step_down(&mut self, term: u64) {
        self.term = term;
        self.voted_for = None;
        self.role = Role::Follower;
        self.votes.clear();
        self.acks.clear();
    }

    fn can_vote_for(&self, candidate: usize) -> bool {
        self.voted_for.is_none() || self.voted_for == Some(candidate)
    }

    fn last_log_term(&self) -> u64 {
        self.log.last().map_or(0, |entry| entry.term)
    }
}

#[derive(Debug)]
struct AcceptedWrite {
    leader: usize,
    term: u64,
    id: u64,
    highest_elected_term_at_accept: u64,
}

#[derive(Default)]
struct Stats {
    elections: usize,
    client_writes: usize,
    partitions: usize,
    drops: usize,
    delays: usize,
    reorders: usize,
    crashes: usize,
}

#[derive(Debug)]
struct ModelError {
    trace_id: String,
    step: usize,
    property: &'static str,
    detail: String,
    trace: Vec<String>,
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} step={} property={} detail={} trace={:?}",
            self.trace_id, self.step, self.property, self.detail, self.trace
        )
    }
}

fn up_to_date(candidate_index: usize, candidate_term: u64, voter_log: &[LogEntry]) -> bool {
    let voter_term = voter_log.last().map_or(0, |entry| entry.term);
    candidate_term > voter_term
        || (candidate_term == voter_term && candidate_index >= voter_log.len())
}

fn parse_node(raw: Option<&str>) -> usize {
    raw.and_then(|value| value.strip_prefix('n'))
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|node| *node < NODE_COUNT)
        .unwrap_or(0)
}

struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    fn node(&mut self) -> usize {
        self.index(NODE_COUNT)
    }

    fn index(&mut self, upper: usize) -> usize {
        (self.next() as usize) % upper
    }

    fn next(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }
}
