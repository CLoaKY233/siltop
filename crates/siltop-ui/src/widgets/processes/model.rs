use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::Hash;

use siltop_core::{ProcSort, Process};

/// Rows kept between the selection and the viewport edge while moving.
pub const SCROLL_MARGIN: usize = 2;

/// Where the selection lives. `Top` follows row 0 so a fresh or re-sorted
/// list always shows its heaviest rows; the user's first move pins a pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Anchor {
    #[default]
    Top,
    Pinned(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ModelRow {
    /// Index into `Snapshot::processes` of the generation the model was built from.
    pub index: usize,
    pub pid: u32,
    pub depth: u8,
    /// Tree view: the connector lines to draw left of the name.
    pub guide: Guide,
    /// Whether the row heads members (tree children or app helpers) and
    /// whether they are shown.
    pub fold: Fold,
    /// Summed values the row stands for: an app group, or a collapsed
    /// subtree. `None` for a row that is just its own process.
    pub agg: Option<Agg>,
}

impl ModelRow {
    /// The process as this row shows it: its own values, or the sums it
    /// stands for.
    #[must_use]
    pub fn shown<'a>(&self, proc: &'a Process) -> std::borrow::Cow<'a, Process> {
        match self.agg {
            Some(agg) => std::borrow::Cow::Owned(agg.onto(proc)),
            None => std::borrow::Cow::Borrowed(proc),
        }
    }
}

/// Whether a row has members, and whether they are listed under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fold {
    #[default]
    Leaf,
    Open,
    Closed,
}

/// Sums over a set of processes. A sum is known when any member's value
/// is; unknown members add nothing rather than a made-up zero.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Agg {
    /// Processes summed, the head row included.
    pub members: u32,
    pub cpu: Option<f32>,
    pub mem: Option<u64>,
    pub energy: Option<f32>,
    pub gpu: f32,
    pub threads: Option<u32>,
}

impl Agg {
    #[must_use]
    pub fn of<'a>(procs: impl IntoIterator<Item = &'a Process>) -> Self {
        fn add<T: std::ops::Add<Output = T>>(a: Option<T>, b: Option<T>) -> Option<T> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a + b),
                (a, b) => a.or(b),
            }
        }
        procs.into_iter().fold(Self::default(), |acc, p| Self {
            members: acc.members + 1,
            cpu: add(acc.cpu, p.cpu.filter(|v| v.is_finite())),
            mem: add(acc.mem, p.mem_bytes),
            energy: add(acc.energy, p.energy_w.filter(|v| v.is_finite())),
            gpu: acc.gpu + p.gpu,
            threads: add(acc.threads, p.threads),
        })
    }

    /// `head` carrying these sums, for sorting and drawing.
    #[must_use]
    pub fn onto(&self, head: &Process) -> Process {
        Process {
            cpu: self.cpu,
            mem_bytes: self.mem,
            energy_w: self.energy,
            gpu: self.gpu,
            threads: self.threads,
            restricted: head.restricted && self.cpu.is_none(),
            ..head.clone()
        }
    }

    /// Members beyond the head row, for the `+N` cue.
    #[must_use]
    pub const fn hidden(&self) -> u32 {
        self.members.saturating_sub(1)
    }
}

/// A set of fold keys over a default: `contains(k)` is `all` flipped for
/// the keys toggled since. Expand-all and collapse-all are one `fill`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldSet<K: Hash + Eq> {
    all: bool,
    flipped: HashSet<K>,
}

impl<K: Hash + Eq> Default for FoldSet<K> {
    fn default() -> Self {
        Self {
            all: false,
            flipped: HashSet::new(),
        }
    }
}

impl<K: Hash + Eq> FoldSet<K> {
    #[must_use]
    pub fn contains(&self, key: &K) -> bool {
        self.all != self.flipped.contains(key)
    }

    pub fn set(&mut self, key: K, on: bool) {
        if on == self.all {
            self.flipped.remove(&key);
        } else {
            self.flipped.insert(key);
        }
    }

    pub fn toggle(&mut self, key: K) {
        let on = !self.contains(&key);
        self.set(key, on);
    }

    pub fn fill(&mut self, on: bool) {
        self.all = on;
        self.flipped.clear();
    }

    /// Forgets keys that no longer exist, so the set never grows.
    pub fn retain(&mut self, keep: impl Fn(&K) -> bool) {
        self.flipped.retain(|k| keep(k));
    }
}

/// How the table nests rows, beyond sort and filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Nesting {
    #[default]
    Flat,
    Tree,
    /// Helpers under their `.app` bundle.
    Apps,
}

/// The fold state a build reads.
#[derive(Debug, Clone, Default)]
pub struct Folds {
    /// Tree nodes whose subtree is collapsed.
    pub closed: FoldSet<u32>,
    /// App bundles whose helpers are listed.
    pub open_apps: FoldSet<String>,
}

/// Deepest tree level that still indents; deeper rows line up with it.
pub(super) const TREE_INDENT_MAX: u8 = 6;

/// Where a tree row sits among its siblings and which ancestor columns
/// continue past it. Bit `l` of `rails` is set when the ancestor at depth
/// `l + 1` has a later sibling, so its column draws `│`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Guide {
    pub rails: u8,
    pub last: bool,
}

impl Guide {
    /// The prefix drawn before a tree row's name: `│ ` or two blanks per
    /// ancestor column, then `├ ` (more siblings follow) or `╰ `. Indent
    /// stops growing at [`TREE_INDENT_MAX`].
    #[must_use]
    pub fn prefix(self, depth: u8) -> String {
        if depth == 0 {
            return String::new();
        }
        let shown = depth.min(TREE_INDENT_MAX);
        let mut out = String::with_capacity(usize::from(shown) * 4);
        for level in 1..shown {
            let rail = self.rails & (1 << (level - 1)) != 0;
            out.push_str(if rail { "│ " } else { "  " });
        }
        out.push_str(if self.last { "╰ " } else { "├ " });
        out
    }

    /// The guide handed to the children of a row at `depth`.
    const fn child(self, depth: u8) -> Self {
        let rails = if depth >= 1 && depth <= 8 && !self.last {
            self.rails | (1 << (depth - 1))
        } else {
            self.rails
        };
        Self { rails, last: false }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelKey {
    generation: u64,
    sort: ProcSort,
    reversed: bool,
    filter: String,
    tree: bool,
    grouped: bool,
    folds: u64,
}

/// The filtered, sorted process order. Rebuilt only when the process
/// generation, sort, filter or tree mode changes; frames read it as is.
#[derive(Debug, Clone, Default)]
pub struct ProcModel {
    key: Option<ModelKey>,
    rows: Vec<ModelRow>,
    /// App group head pid → every member pid, the head included.
    members: HashMap<u32, Vec<u32>>,
    rebuilds: u64,
}

impl ProcModel {
    #[must_use]
    pub fn build(
        processes: &[Process],
        filter: &str,
        sort: ProcSort,
        reversed: bool,
        tree: bool,
    ) -> Self {
        let nesting = if tree { Nesting::Tree } else { Nesting::Flat };
        Self::build_nested(
            processes,
            filter,
            sort,
            reversed,
            nesting,
            &Folds::default(),
        )
    }

    #[must_use]
    pub fn build_nested(
        processes: &[Process],
        filter: &str,
        sort: ProcSort,
        reversed: bool,
        nesting: Nesting,
        folds: &Folds,
    ) -> Self {
        let order = Order { sort, reversed };
        let (rows, members) = match nesting {
            Nesting::Flat => (flat_rows(processes, filter, order), HashMap::new()),
            Nesting::Tree => (
                tree_rows(processes, filter, order, &folds.closed),
                HashMap::new(),
            ),
            Nesting::Apps => app_rows(processes, filter, order, &folds.open_apps),
        };
        Self {
            key: None,
            rows,
            members,
            rebuilds: 0,
        }
    }

    /// Every pid a row stands for: an app group's members, else itself.
    #[must_use]
    pub fn members_of(&self, pid: u32) -> Vec<u32> {
        self.members.get(&pid).cloned().unwrap_or_else(|| vec![pid])
    }

    fn is_for(&self, want: &ModelKey) -> bool {
        self.key.as_ref() == Some(want)
    }

    #[must_use]
    pub fn rows(&self) -> &[ModelRow] {
        &self.rows
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    #[must_use]
    pub fn position(&self, pid: u32) -> Option<usize> {
        self.rows.iter().position(|row| row.pid == pid)
    }

    #[must_use]
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds
    }
}

/// Pids tagged with space, each with the start time of the process it
/// tagged, so a pid reused by another process never inherits the tag.
#[derive(Debug, Clone, Default)]
pub struct Tags(BTreeMap<u32, i64>);

impl Tags {
    #[must_use]
    pub fn contains(&self, pid: u32) -> bool {
        self.0.contains_key(&pid)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// The tagged pids, ascending.
    pub fn pids(&self) -> impl Iterator<Item = u32> + '_ {
        self.0.keys().copied()
    }

    /// Keeps the tags whose pid `keep` accepts.
    pub fn retain(&mut self, mut keep: impl FnMut(u32) -> bool) {
        self.0.retain(|&pid, _| keep(pid));
    }
}

/// Selection, scroll offset, filter and the cached model for the table.
/// The app owns it; widgets read it and never recompute the order.
#[derive(Debug, Clone, Default)]
pub struct ProcView {
    pub anchor: Anchor,
    /// First model row in the viewport. Kept across frames and samples:
    /// only the user (keys, wheel, a reset) moves it, so re-sorting never
    /// scrolls the list out from under the pointer.
    pub offset: usize,
    pub filter: String,
    /// Bumped by the app whenever `Snapshot::processes` is replaced.
    pub generation: u64,
    pub model: ProcModel,
    /// Helpers listed under their `.app` bundle (`a`).
    pub grouped: bool,
    /// Collapsed tree nodes and opened app groups; survive refreshes.
    pub folds: Folds,
    /// Bumped on every fold change so the model rebuilds.
    pub fold_rev: u64,
    /// Pids tagged with space; an action on the table targets them all.
    pub tags: Tags,
    /// Every pid the armed confirm or open picker targets, when more than
    /// one, so the prompt can list them.
    pub armed: Vec<u32>,
    /// The dossier's open page.
    pub tab: super::dossier::DossierTab,
    /// First listed row of the open threads, files or ports page. Reset
    /// when the page or the pid changes; clamped by `scroll_page`.
    pub page_offset: usize,
    /// Threads, files and sockets of the dossier's pid, read on demand
    /// while a page that shows them is open.
    pub inspection: Option<siltop_core::Inspection>,
    /// Situational columns (gpu%, read, write, ...) any sample so far had
    /// a value for. They keep their cells for the session: a column set
    /// that changed with each tick moved the header under the pointer, so
    /// a click on `mem` sorted by a column that then vanished.
    pub columns_seen: u32,
}

impl ProcView {
    fn key(&self, sort: ProcSort, reversed: bool, tree: bool) -> ModelKey {
        ModelKey {
            generation: self.generation,
            sort,
            reversed,
            filter: self.filter.to_ascii_lowercase(),
            tree,
            grouped: self.grouped,
            folds: self.fold_rev,
        }
    }

    /// Tree wins over grouping when both are on: nesting by parent is the
    /// more specific ask.
    #[must_use]
    pub const fn nesting(&self, tree: bool) -> Nesting {
        if tree {
            Nesting::Tree
        } else if self.grouped {
            Nesting::Apps
        } else {
            Nesting::Flat
        }
    }

    fn build(&self, processes: &[Process], key: &ModelKey) -> ProcModel {
        ProcModel::build_nested(
            processes,
            &key.filter,
            key.sort,
            key.reversed,
            self.nesting(key.tree),
            &self.folds,
        )
    }

    /// Rebuilds the model if its key changed; returns whether it did.
    pub fn sync(
        &mut self,
        processes: &[Process],
        sort: ProcSort,
        reversed: bool,
        tree: bool,
    ) -> bool {
        let key = self.key(sort, reversed, tree);
        if self.model.is_for(&key) {
            return false;
        }
        let rebuilds = self.model.rebuilds + 1;
        self.model = self.build(processes, &key);
        self.model.rebuilds = rebuilds;
        self.model.key = Some(key);
        true
    }

    /// The model to draw: the cached one when it is current, else a fresh build.
    #[must_use]
    pub fn current(
        &self,
        processes: &[Process],
        sort: ProcSort,
        reversed: bool,
        tree: bool,
    ) -> std::borrow::Cow<'_, ProcModel> {
        let key = self.key(sort, reversed, tree);
        if self.model.is_for(&key) {
            std::borrow::Cow::Borrowed(&self.model)
        } else {
            std::borrow::Cow::Owned(self.build(processes, &key))
        }
    }

    /// The selected row of the synced model.
    #[must_use]
    pub fn selected(&self) -> Option<&ModelRow> {
        self.selected_row(&self.model)
            .and_then(|at| self.model.rows.get(at))
    }

    /// Opens (`Some(true)`), closes (`Some(false)`) or toggles (`None`)
    /// the selected row's members. Returns whether anything changed; a
    /// leaf row has nothing to fold.
    pub fn fold_selected(&mut self, open: Option<bool>, processes: &[Process]) -> bool {
        let Some(row) = self.selected().copied() else {
            return false;
        };
        if row.fold == Fold::Leaf {
            return false;
        }
        let open = open.unwrap_or(row.fold == Fold::Closed);
        if open == (row.fold == Fold::Open) {
            return false;
        }
        if self.grouped && self.model.members.contains_key(&row.pid) {
            let Some(bundle) = processes.get(row.index).and_then(app_bundle) else {
                return false;
            };
            self.folds.open_apps.set(bundle.to_owned(), open);
        } else {
            self.folds.closed.set(row.pid, !open);
        }
        self.fold_rev += 1;
        true
    }

    /// Expand-all (`true`) or collapse-all, in the current nesting.
    pub fn fold_all(&mut self, open: bool) {
        self.folds.closed.fill(!open);
        self.folds.open_apps.fill(open);
        self.fold_rev += 1;
    }

    /// Tags every pid in `pids`, or untags them all when all were tagged.
    /// Each tag remembers the start time of the process in `processes`
    /// (the snapshot on screen) that holds the pid.
    pub fn toggle_tags(&mut self, pids: &[u32], processes: &[Process]) {
        if pids.iter().all(|&p| self.tags.contains(p)) {
            for &pid in pids {
                self.tags.0.remove(&pid);
            }
        } else {
            for proc in processes.iter().filter(|p| pids.contains(&p.pid)) {
                self.tags.0.insert(proc.pid, proc.start_unix);
            }
        }
    }

    /// After a new sample: tags and folds of pids that exited are dropped,
    /// and so is a tag whose pid a new process took over (a long freeze
    /// can span a pid wrap), so x never aims at a process nobody tagged;
    /// the columns it has values for join [`Self::columns_seen`].
    pub fn prune(&mut self, processes: &[Process]) {
        self.columns_seen |= super::columns::sticky(processes);
        let alive: HashMap<u32, i64> = processes.iter().map(|p| (p.pid, p.start_unix)).collect();
        self.tags
            .0
            .retain(|pid, start| alive.get(pid) == Some(start));
        let alive: HashSet<u32> = alive.into_keys().collect();
        self.folds.closed.retain(|pid| alive.contains(pid));
        self.armed.retain(|pid| alive.contains(pid));
    }

    #[must_use]
    pub fn selected_row(&self, model: &ProcModel) -> Option<usize> {
        match self.anchor {
            Anchor::Top => (!model.is_empty()).then_some(0),
            Anchor::Pinned(pid) => model.position(pid),
        }
    }

    #[must_use]
    pub fn selected_pid(&self) -> Option<u32> {
        let row = self.selected_row(&self.model)?;
        self.model.rows.get(row).map(|r| r.pid)
    }

    pub fn reset_top(&mut self) {
        self.anchor = Anchor::Top;
        self.offset = 0;
    }

    /// Keyboard move: pins the pid it lands on and keeps it in view.
    pub fn move_by(&mut self, delta: i32, rows: usize) {
        let len = self.model.len();
        if len == 0 {
            self.reset_top();
            return;
        }
        let from = self.selected_row(&self.model).unwrap_or(0);
        let to = i64::try_from(from).unwrap_or(0) + i64::from(delta);
        let to = usize::try_from(to.max(0)).unwrap_or(0).min(len - 1);
        self.select(to, rows);
        self.reveal(to, rows, SCROLL_MARGIN);
    }

    /// Click: pins the row under the pointer. It is already on screen, so
    /// the viewport does not move.
    pub fn select(&mut self, row: usize, rows: usize) {
        if let Some(target) = self.model.rows.get(row) {
            self.anchor = Anchor::Pinned(target.pid);
            self.reveal(row, rows, 0);
        }
    }

    /// Wheel: moves the viewport only.
    pub fn scroll_by(&mut self, delta: i32, rows: usize) {
        let max = max_offset(self.model.len(), rows);
        let to = i64::try_from(self.offset).unwrap_or(0) + i64::from(delta);
        self.offset = usize::try_from(to.max(0)).unwrap_or(0).min(max);
    }

    /// Clamps the offset after the list or the viewport changed size.
    pub fn settle(&mut self, rows: usize) {
        self.offset = self.offset.min(max_offset(self.model.len(), rows));
    }

    /// Scrolls the least that puts `row` on screen with `margin` rows of
    /// context on the side it moved toward.
    fn reveal(&mut self, row: usize, rows: usize, margin: usize) {
        if rows == 0 {
            return;
        }
        let margin = margin.min(rows.saturating_sub(1) / 2);
        let len = self.model.len();
        if row < self.offset + margin {
            self.offset = row.saturating_sub(margin);
        } else if row + margin >= self.offset + rows {
            self.offset = (row + margin + 1).saturating_sub(rows);
        }
        self.offset = self.offset.min(max_offset(len, rows));
    }
}

#[must_use]
pub(super) fn max_offset(len: usize, rows: usize) -> usize {
    len.saturating_sub(rows)
}

/// One row's value under a sort key, at the precision the table shows it
/// so rows that draw the same never trade places between frames.
enum Key<'a> {
    Num(Option<i64>),
    Text(Option<&'a str>),
}

fn key(proc: &Process, sort: ProcSort) -> Key<'_> {
    let tenths = |v: Option<f32>| {
        v.filter(|v| v.is_finite())
            .map(|v| (f64::from(v) * 10.0).round() as i64)
    };
    let rate = |v: Option<f64>| v.filter(|v| v.is_finite()).map(|v| v.round() as i64);
    let bytes = |v: Option<u64>| v.map(|v| i64::try_from(v).unwrap_or(i64::MAX));
    match sort {
        ProcSort::Cpu => Key::Num(tenths(proc.cpu)),
        ProcSort::Mem => Key::Num(bytes(proc.mem_bytes)),
        ProcSort::Rss => Key::Num(bytes(proc.rss_bytes)),
        ProcSort::Pid => Key::Num(Some(i64::from(proc.pid))),
        ProcSort::Threads => Key::Num(proc.threads.map(i64::from)),
        ProcSort::Gpu => Key::Num(tenths(Some(proc.gpu))),
        ProcSort::Power => Key::Num(tenths(proc.energy_w)),
        ProcSort::DiskRead => Key::Num(rate(proc.disk_read_bps)),
        ProcSort::DiskWrite => Key::Num(rate(proc.disk_write_bps)),
        ProcSort::Wakeups => Key::Num(tenths(proc.wakeups_per_s).map(|t| (t + 5) / 10)),
        ProcSort::Started => Key::Num((proc.start_unix > 0).then_some(proc.start_unix)),
        ProcSort::Name => Key::Text(Some(proc.name.as_str())),
        ProcSort::User => Key::Text((!proc.user.is_empty()).then_some(proc.user.as_str())),
        ProcSort::State => Key::Text(Some(proc.status)),
    }
}

/// The sort key first, then memory, then pid, so ties never flicker.
///
/// Unknown values (`None`: a restricted process) sort last in both
/// directions, so reversing never floods the top with `—` rows.
fn compare(a: &Process, b: &Process, sort: ProcSort, reversed: bool) -> Ordering {
    let descending = sort.natural_descending() != reversed;
    let primary = match (key(a, sort), key(b, sort)) {
        (Key::Num(a), Key::Num(b)) => known_first(a, b, descending, Ord::cmp),
        (Key::Text(a), Key::Text(b)) => known_first(a, b, descending, |a, b| {
            fold(a).cmp(fold(b)).then_with(|| a.cmp(b))
        }),
        _ => Ordering::Equal,
    };
    primary
        .then_with(|| known_first(a.mem_bytes, b.mem_bytes, true, Ord::cmp))
        .then_with(|| a.pid.cmp(&b.pid))
}

/// ASCII case-folded bytes, for text keys.
fn fold(text: &str) -> impl Iterator<Item = u8> + '_ {
    text.bytes().map(|c| c.to_ascii_lowercase())
}

/// Orders known values by `cmp` (reversed when `descending`), with `None`
/// after every value either way.
fn known_first<T>(
    a: Option<T>,
    b: Option<T>,
    descending: bool,
    cmp: impl Fn(&T, &T) -> Ordering,
) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => {
            let order = cmp(&a, &b);
            if descending { order.reverse() } else { order }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[derive(Debug, Clone, Copy)]
struct Order {
    sort: ProcSort,
    reversed: bool,
}

impl Order {
    fn cmp(self, a: &Process, b: &Process) -> Ordering {
        compare(a, b, self.sort, self.reversed)
    }

    fn sort(self, processes: &[Process], ids: &mut [usize]) {
        ids.sort_by(|&a, &b| self.cmp(&processes[a], &processes[b]));
    }
}

fn leaf(processes: &[Process], index: usize, depth: u8, guide: Guide) -> ModelRow {
    ModelRow {
        index,
        pid: processes[index].pid,
        depth,
        guide,
        ..ModelRow::default()
    }
}

fn flat_rows(processes: &[Process], needle: &str, order: Order) -> Vec<ModelRow> {
    let mut picked: Vec<usize> = processes
        .iter()
        .enumerate()
        .filter(|(_, proc)| matches_filter(proc, needle))
        .map(|(i, _)| i)
        .collect();
    order.sort(processes, &mut picked);
    picked
        .into_iter()
        .map(|index| leaf(processes, index, 0, Guide::default()))
        .collect()
}

/// The `.app` bundle a process runs from: its executable path up to the
/// first `.app/`, so a helper nested in `Foo.app/Contents/Frameworks/…`
/// belongs to `Foo.app`. `None` outside any bundle, and for a bundle
/// inside a `.framework` (an interpreter such as Python.app runs every
/// script, which are separate programs, not helpers of one app).
///
/// A bundle's `Contents/Developer/` is a toolchain, not the app: Xcode
/// ships git, clang and python3 there, and every terminal run of them is a
/// separate program. Such paths look past the outer bundle, so git stays
/// ungrouped, Xcode's Python.app meets the `.framework` rule, and a real
/// app shipped inside (Simulator.app) is still its own bundle.
#[must_use]
pub(super) fn app_bundle(proc: &Process) -> Option<&str> {
    const TOOLCHAIN: &str = "/Contents/Developer/";
    let path = proc
        .command
        .as_deref()
        .filter(|c| c.starts_with('/'))
        .or_else(|| proc.args.first().map(String::as_str))?;
    let mut end = 0;
    loop {
        end += path[end..].find(".app/")? + ".app".len();
        if !path[end..].starts_with(TOOLCHAIN) {
            break;
        }
    }
    let bundle = &path[..end];
    (!bundle.contains(".framework/")).then_some(bundle)
}

/// A bundle's head process: the one running the bundle's own executable
/// (no nested `.app` after it), lowest pid first; else the lowest pid.
fn group_head(processes: &[Process], bundle: &str, members: &[usize]) -> Option<usize> {
    let nested = |i: usize| {
        app_bundle(&processes[i])
            .zip(processes[i].command.as_deref())
            .is_some_and(|(_, cmd)| {
                cmd.get(bundle.len()..)
                    .is_some_and(|rest| rest.contains(".app/"))
            })
    };
    members
        .iter()
        .copied()
        .min_by_key(|&i| (nested(i), processes[i].pid))
}

type AppRows = (Vec<ModelRow>, HashMap<u32, Vec<u32>>);

/// One sortable entry of the grouped list: a lone process, or an app's
/// head carrying its members' sums.
struct Entry<'a> {
    /// What sorting compares: the process, or the head with the sums.
    rep: Process,
    head: usize,
    group: Option<Group<'a>>,
}

struct Group<'a> {
    bundle: &'a str,
    helpers: Vec<usize>,
    agg: Agg,
}

/// Rows grouped under their `.app` bundle. A group row is its head process
/// carrying the members' sums; sort orders groups by those sums, and
/// helpers by their own values under an open group.
fn app_rows(processes: &[Process], needle: &str, order: Order, open: &FoldSet<String>) -> AppRows {
    let mut bundles: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    let mut entries: Vec<Entry<'_>> = Vec::new();
    let lone = |head: usize| Entry {
        rep: processes[head].clone(),
        head,
        group: None,
    };
    for (i, proc) in processes.iter().enumerate() {
        if !matches_filter(proc, needle) {
            continue;
        }
        match app_bundle(proc) {
            Some(bundle) => bundles.entry(bundle).or_default().push(i),
            None => entries.push(lone(i)),
        }
    }
    for (bundle, members) in bundles {
        let Some(head) = group_head(processes, bundle, &members) else {
            continue;
        };
        if members.len() == 1 {
            entries.push(lone(head));
            continue;
        }
        let agg = Agg::of(members.iter().map(|&i| &processes[i]));
        let helpers: Vec<usize> = members.into_iter().filter(|&i| i != head).collect();
        entries.push(Entry {
            rep: agg.onto(&processes[head]),
            head,
            group: Some(Group {
                bundle,
                helpers,
                agg,
            }),
        });
    }
    entries.sort_by(|a, b| order.cmp(&a.rep, &b.rep));

    let mut rows = Vec::with_capacity(entries.len());
    let mut members = HashMap::new();
    for Entry { head, group, .. } in entries {
        let Some(Group {
            bundle,
            mut helpers,
            agg,
        }) = group
        else {
            rows.push(leaf(processes, head, 0, Guide::default()));
            continue;
        };
        let is_open = open.contains(&bundle.to_owned());
        rows.push(ModelRow {
            fold: if is_open { Fold::Open } else { Fold::Closed },
            agg: Some(agg),
            ..leaf(processes, head, 0, Guide::default())
        });
        members.insert(
            processes[head].pid,
            std::iter::once(head)
                .chain(helpers.iter().copied())
                .map(|i| processes[i].pid)
                .collect(),
        );
        if is_open {
            order.sort(processes, &mut helpers);
            let n = helpers.len();
            rows.extend(helpers.into_iter().enumerate().map(|(at, i)| {
                let guide = Guide {
                    rails: 0,
                    last: at + 1 == n,
                };
                leaf(processes, i, 1, guide)
            }));
        }
    }
    (rows, members)
}

fn tree_rows(
    processes: &[Process],
    needle: &str,
    order: Order,
    closed: &FoldSet<u32>,
) -> Vec<ModelRow> {
    let by_pid: HashMap<u32, usize> = processes
        .iter()
        .enumerate()
        .map(|(i, p)| (p.pid, i))
        .collect();
    let mut children: HashMap<u32, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();
    for (i, proc) in processes.iter().enumerate() {
        if proc.ppid == 0 || proc.ppid == proc.pid || !by_pid.contains_key(&proc.ppid) {
            roots.push(i);
        } else {
            children.entry(proc.ppid).or_default().push(i);
        }
    }

    let keep = (!needle.is_empty()).then(|| {
        let mut keep = HashSet::new();
        for proc in processes {
            if !matches_filter(proc, needle) {
                continue;
            }
            let mut walk = Some(proc.pid);
            while let Some(pid) = walk {
                if !keep.insert(pid) {
                    break;
                }
                walk = by_pid
                    .get(&pid)
                    .and_then(|&i| processes.get(i))
                    .and_then(|p| (p.ppid != 0 && p.ppid != p.pid).then_some(p.ppid));
            }
        }
        keep
    });

    order.sort(processes, &mut roots);
    for kids in children.values_mut() {
        order.sort(processes, kids);
    }

    let kept = |i: &usize| {
        keep.as_ref()
            .is_none_or(|keep| keep.contains(&processes[*i].pid))
    };
    let visible_kids = |pid: u32| -> Vec<usize> {
        children
            .get(&pid)
            .map(|kids| kids.iter().copied().filter(kept).collect())
            .unwrap_or_default()
    };
    // Every listed descendant of `pid`, for a collapsed node's totals.
    let subtree = |pid: u32| -> Vec<usize> {
        let mut out = Vec::new();
        let mut walk = visible_kids(pid);
        while let Some(i) = walk.pop() {
            out.push(i);
            walk.extend(visible_kids(processes[i].pid));
        }
        out
    };
    let push_level =
        |stack: &mut Vec<(usize, u8, Guide)>, ids: &[usize], depth: u8, parent: Guide| {
            let visible: Vec<usize> = ids.iter().copied().filter(kept).collect();
            let n = visible.len();
            for (at, index) in visible.into_iter().enumerate().rev() {
                let guide = Guide {
                    last: at + 1 == n,
                    ..parent
                };
                stack.push((index, depth, guide));
            }
        };
    let mut out = Vec::new();
    let mut stack: Vec<(usize, u8, Guide)> = Vec::new();
    push_level(&mut stack, &roots, 0, Guide::default());
    while let Some((index, depth, guide)) = stack.pop() {
        let pid = processes[index].pid;
        let kids = visible_kids(pid);
        let mut row = leaf(processes, index, depth, guide);
        if kids.is_empty() {
            out.push(row);
        } else if closed.contains(&pid) {
            let below = subtree(pid);
            row.fold = Fold::Closed;
            row.agg = Some(Agg::of(
                std::iter::once(index).chain(below).map(|i| &processes[i]),
            ));
            out.push(row);
        } else {
            row.fold = Fold::Open;
            out.push(row);
            push_level(
                &mut stack,
                &kids,
                depth.saturating_add(1),
                guide.child(depth),
            );
        }
    }
    out
}

#[must_use]
pub(super) fn matches_filter(proc: &Process, needle: &str) -> bool {
    needle.is_empty()
        || contains_ignore_ascii(&proc.name, needle)
        || u32_contains(proc.pid, needle)
        || proc
            .command
            .as_ref()
            .is_some_and(|c| contains_ignore_ascii(c, needle))
}

fn contains_ignore_ascii(hay: &str, needle: &str) -> bool {
    hay.to_ascii_lowercase().contains(needle)
}

fn u32_contains(n: u32, needle: &str) -> bool {
    if needle.is_empty() || !needle.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    n.to_string().contains(needle)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::widgets::tests_support::process;

    fn names(view: &ProcView, processes: &[Process]) -> Vec<String> {
        view.model
            .rows()
            .iter()
            .map(|row| processes[row.index].name.clone())
            .collect()
    }

    fn many(n: u32) -> Vec<Process> {
        (1..=n)
            .map(|pid| process(pid, &format!("p{pid}"), n as f32 - pid as f32))
            .collect()
    }

    #[test]
    fn rebuilds_only_when_the_key_changes() {
        let procs = many(10);
        let mut view = ProcView::default();
        assert!(view.sync(&procs, ProcSort::Cpu, false, false));
        for _ in 0..5 {
            assert!(!view.sync(&procs, ProcSort::Cpu, false, false));
        }
        assert_eq!(view.model.rebuilds(), 1);
        view.filter = String::from("p1");
        assert!(view.sync(&procs, ProcSort::Cpu, false, false));
        assert!(view.sync(&procs, ProcSort::Mem, false, false));
        assert!(view.sync(&procs, ProcSort::Mem, true, false));
        assert!(view.sync(&procs, ProcSort::Mem, true, true));
        view.generation += 1;
        assert!(view.sync(&procs, ProcSort::Mem, true, true));
        assert!(!view.sync(&procs, ProcSort::Mem, true, true));
        assert_eq!(view.model.rebuilds(), 6);
    }

    #[test]
    fn equal_cpu_rows_keep_their_order_across_frames() {
        let mut procs: Vec<Process> = (1..=6).map(|pid| process(pid, "twin", 12.34)).collect();
        for (i, proc) in procs.iter_mut().enumerate() {
            proc.mem_bytes = Some(1000 - (i as u64 % 3) * 100);
        }
        let mut view = ProcView::default();
        view.sync(&procs, ProcSort::Cpu, false, false);
        let first: Vec<u32> = view.model.rows().iter().map(|r| r.pid).collect();
        for frame in 0..10_u32 {
            // jitter below the displayed precision must not reorder rows
            for proc in &mut procs {
                proc.cpu = Some(12.34 + (frame % 3) as f32 * 0.001);
            }
            view.generation += 1;
            view.sync(&procs, ProcSort::Cpu, false, false);
            let now: Vec<u32> = view.model.rows().iter().map(|r| r.pid).collect();
            assert_eq!(now, first, "frame {frame}");
        }
        assert_eq!(first, vec![1, 4, 2, 5, 3, 6], "mem desc, then pid asc");
    }

    fn unknown(pid: u32) -> Process {
        Process {
            cpu: None,
            mem_bytes: None,
            threads: None,
            restricted: true,
            ..process(pid, &format!("r{pid}"), 0.0)
        }
    }

    #[test]
    fn d23_unknown_cpu_sorts_last_in_both_directions() {
        let procs = vec![
            unknown(1),
            process(2, "busy", 50.0),
            unknown(3),
            process(4, "idle", 0.0),
        ];
        let mut view = ProcView::default();
        view.sync(&procs, ProcSort::Cpu, false, false);
        assert_eq!(names(&view, &procs), ["busy", "idle", "r1", "r3"]);
        view.sync(&procs, ProcSort::Cpu, true, false);
        assert_eq!(names(&view, &procs), ["idle", "busy", "r1", "r3"]);
    }

    #[test]
    fn d23_unknown_mem_sorts_last_in_both_directions() {
        let mut procs = vec![unknown(1), process(2, "big", 1.0), process(3, "small", 1.0)];
        procs[1].mem_bytes = Some(900);
        procs[2].mem_bytes = Some(10);
        let mut view = ProcView::default();
        view.sync(&procs, ProcSort::Mem, false, false);
        assert_eq!(names(&view, &procs), ["big", "small", "r1"]);
        view.sync(&procs, ProcSort::Mem, true, false);
        assert_eq!(names(&view, &procs), ["small", "big", "r1"]);
    }

    #[test]
    fn nan_cpu_is_treated_as_unknown() {
        let mut procs = vec![process(1, "nan", 0.0), process(2, "ok", 1.0)];
        procs[0].cpu = Some(f32::NAN);
        let mut view = ProcView::default();
        view.sync(&procs, ProcSort::Cpu, false, false);
        assert_eq!(names(&view, &procs), ["ok", "nan"]);
    }

    #[test]
    fn known_before_unknown_sweep() {
        let mut seed = 0x2545_f491_u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for round in 0..60 {
            let procs: Vec<Process> = (1..=40)
                .map(|pid| {
                    if next() % 3 == 0 {
                        unknown(pid)
                    } else {
                        let mut p = process(pid, "k", (next() % 4000) as f32 / 10.0);
                        p.mem_bytes = Some(u64::from(next() % 5000));
                        p
                    }
                })
                .collect();
            for sort in [ProcSort::Cpu, ProcSort::Mem] {
                for reversed in [false, true] {
                    let mut view = ProcView::default();
                    view.sync(&procs, sort, reversed, false);
                    let known: Vec<bool> = view
                        .model
                        .rows()
                        .iter()
                        .map(|r| procs[r.index].cpu.is_some())
                        .collect();
                    let first_unknown = known.iter().position(|k| !k).unwrap_or(known.len());
                    assert!(
                        known[first_unknown..].iter().all(|k| !k),
                        "round {round} {sort:?} reversed={reversed}"
                    );
                    assert_eq!(view.model.len(), procs.len());
                }
            }
        }
    }

    #[test]
    fn reversed_flips_only_the_primary_key() {
        let procs = vec![
            process(3, "c", 1.0),
            process(1, "a", 1.0),
            process(2, "b", 5.0),
        ];
        let mut view = ProcView::default();
        view.sync(&procs, ProcSort::Pid, false, false);
        assert_eq!(names(&view, &procs), ["a", "b", "c"]);
        view.sync(&procs, ProcSort::Pid, true, false);
        assert_eq!(names(&view, &procs), ["c", "b", "a"]);
        view.sync(&procs, ProcSort::Cpu, true, false);
        assert_eq!(
            names(&view, &procs),
            ["a", "c", "b"],
            "ties still break by pid"
        );
    }

    #[test]
    fn top_anchor_follows_row_zero_across_resorts() {
        let mut procs = many(5);
        let mut view = ProcView::default();
        view.sync(&procs, ProcSort::Cpu, false, false);
        assert_eq!(view.selected_pid(), Some(1));
        procs[4].cpu = Some(99.0);
        view.generation += 1;
        view.sync(&procs, ProcSort::Cpu, false, false);
        assert_eq!(
            view.selected_pid(),
            Some(5),
            "top follows the new heaviest row"
        );
    }

    #[test]
    fn a_resort_moves_the_highlight_not_the_viewport() {
        let mut procs = many(100);
        let mut view = ProcView::default();
        view.sync(&procs, ProcSort::Cpu, false, false);
        view.scroll_by(20, 20);
        view.select(25, 20);
        procs[25].cpu = Some(-1.0);
        view.generation += 1;
        view.sync(&procs, ProcSort::Cpu, false, false);
        view.settle(20);
        assert_eq!(view.offset, 20);
        assert_eq!(view.selected_row(&view.model), Some(99));
    }

    #[test]
    fn a_move_pins_the_pid() {
        let mut procs = many(5);
        let mut view = ProcView::default();
        view.sync(&procs, ProcSort::Cpu, false, false);
        view.move_by(1, 10);
        assert_eq!(view.anchor, Anchor::Pinned(2));
        procs[1].cpu = Some(99.0);
        view.generation += 1;
        view.sync(&procs, ProcSort::Cpu, false, false);
        assert_eq!(view.selected_pid(), Some(2));
        assert_eq!(view.selected_row(&view.model), Some(0));
    }

    #[test]
    fn empty_list_has_no_selection() {
        let procs = many(3);
        let mut view = ProcView {
            filter: String::from("zzz"),
            ..ProcView::default()
        };
        view.sync(&procs, ProcSort::Cpu, false, false);
        assert!(view.model.is_empty());
        assert_eq!(view.selected_pid(), None);
        view.move_by(1, 10);
        assert_eq!(view.anchor, Anchor::Top);
        assert_eq!(view.selected_pid(), None);
    }

    #[test]
    fn viewport_stays_still_inside_the_margin() {
        let procs = many(200);
        let mut view = ProcView::default();
        view.sync(&procs, ProcSort::Cpu, false, false);
        let rows = 20;
        for _ in 0..40 {
            view.move_by(1, rows);
        }
        assert_eq!(view.selected_row(&view.model), Some(40));
        let offset = view.offset;
        assert_eq!(offset, 40 + SCROLL_MARGIN + 1 - rows);
        for _ in 0..3 {
            view.move_by(-1, rows);
            assert_eq!(view.offset, offset, "k inside the viewport does not scroll");
        }
        for _ in 0..30 {
            view.move_by(-1, rows);
        }
        assert_eq!(view.selected_row(&view.model), Some(7));
        assert_eq!(view.offset, 7 - SCROLL_MARGIN);
    }

    #[test]
    fn wheel_scrolls_the_viewport_not_the_selection() {
        let procs = many(100);
        let mut view = ProcView::default();
        view.sync(&procs, ProcSort::Cpu, false, false);
        view.scroll_by(30, 20);
        assert_eq!(view.offset, 30);
        assert_eq!(view.selected_pid(), Some(1));
        view.settle(20);
        assert_eq!(view.offset, 30, "a new sample does not scroll");
        view.scroll_by(500, 20);
        assert_eq!(view.offset, 80, "clamped to the last page");
        view.move_by(1, 20);
        assert_eq!(view.offset, 0, "keyboard brings the selection back");
    }

    #[test]
    fn settle_clamps_when_the_list_shrinks() {
        let procs = many(100);
        let mut view = ProcView::default();
        view.sync(&procs, ProcSort::Cpu, false, false);
        view.scroll_by(80, 20);
        view.filter = String::from("p9");
        view.sync(&procs, ProcSort::Cpu, false, false);
        view.settle(20);
        assert_eq!(view.offset, 0);
    }

    /// Rows with a distinct, known value under every key, plus a twin of
    /// row 0 (same values, higher pid) and a restricted row.
    fn every_key_fixture() -> Vec<Process> {
        let mut procs: Vec<Process> = (0..5_u32)
            .map(|i| {
                let v = i + 1;
                Process {
                    pid: 100 + v,
                    ppid: 1,
                    name: format!("n{v}"),
                    user: format!("u{v}"),
                    status: ["", "stopped", "zombie", "a", "b"][i as usize],
                    cpu: Some(v as f32),
                    mem_bytes: Some(u64::from(v) * 1000),
                    rss_bytes: Some(u64::from(v) * 7),
                    threads: Some(v),
                    gpu: v as f32,
                    energy_w: Some(v as f32),
                    disk_read_bps: Some(f64::from(v) * 100.0),
                    disk_write_bps: Some(f64::from(v) * 50.0),
                    wakeups_per_s: Some(v as f32 * 3.0),
                    start_unix: i64::from(v) * 1000,
                    ..Process::default()
                }
            })
            .collect();
        let mut twin = procs[0].clone();
        twin.pid = 900;
        procs.push(twin);
        procs.push(Process {
            pid: 50,
            name: String::from("n9"),
            status: "zz",
            restricted: true,
            ..Process::default()
        });
        procs
    }

    fn order(procs: &[Process], sort: ProcSort, reversed: bool) -> Vec<u32> {
        ProcModel::build(procs, "", sort, reversed, false)
            .rows()
            .iter()
            .map(|r| r.pid)
            .collect()
    }

    #[test]
    fn every_column_sorts_both_directions_with_stable_tie_breaks() {
        let procs = every_key_fixture();
        let text = [ProcSort::Name, ProcSort::User, ProcSort::State];
        for sort in ProcSort::ALL {
            let fwd = order(&procs, sort, false);
            let rev = order(&procs, sort, true);
            let known = |pids: &[u32]| -> Vec<u32> {
                pids.iter()
                    .copied()
                    .filter(|p| *p != 50 && *p != 900)
                    .collect()
            };
            let ascending = vec![101, 102, 103, 104, 105];
            let descending: Vec<u32> = ascending.iter().rev().copied().collect();
            if matches!(sort, ProcSort::Pid) {
                assert_eq!(fwd, [50, 101, 102, 103, 104, 105, 900]);
                assert_eq!(rev, [900, 105, 104, 103, 102, 101, 50]);
                continue;
            }
            let (want_fwd, want_rev) = if sort.natural_descending() {
                (descending.clone(), ascending.clone())
            } else {
                (ascending.clone(), descending.clone())
            };
            if matches!(sort, ProcSort::State) {
                // "", "a", "b", "stopped", "zombie": text order, not pid order
                assert_eq!(known(&fwd), [101, 104, 105, 102, 103], "{sort:?}");
                assert_eq!(known(&rev), [103, 102, 105, 104, 101], "{sort:?}");
            } else if !matches!(sort, ProcSort::Name) {
                assert_eq!(known(&fwd), want_fwd, "{sort:?} forward");
                assert_eq!(known(&rev), want_rev, "{sort:?} reversed");
            }
            // The twin ties row 101 on the key and on memory: pid breaks it,
            // the same way in both directions.
            for pids in [&fwd, &rev] {
                let a = pids.iter().position(|p| *p == 101).unwrap();
                let b = pids.iter().position(|p| *p == 900).unwrap();
                assert_eq!(b, a + 1, "{sort:?}: twin follows by pid in {pids:?}");
            }
            // A restricted row's GPU share is readable (0 here), so it
            // sorts by value instead of last.
            if !text.contains(&sort) && sort != ProcSort::Gpu {
                assert_eq!(fwd.last(), Some(&50), "{sort:?}: restricted last");
                assert_eq!(rev.last(), Some(&50), "{sort:?}: restricted last reversed");
            }
        }
    }

    #[test]
    fn name_sort_is_case_insensitive_and_alphabetical() {
        let procs = vec![
            process(1, "zsh", 0.0),
            process(2, "Arc", 0.0),
            process(3, "arc", 0.0),
            process(4, "Finder", 0.0),
        ];
        assert_eq!(order(&procs, ProcSort::Name, false), [2, 3, 4, 1]);
        assert_eq!(order(&procs, ProcSort::Name, true), [1, 4, 3, 2]);
    }

    #[test]
    fn restricted_gpu_sorts_by_value_and_empty_user_sorts_last() {
        let mut procs = vec![unknown(1), process(2, "a", 1.0), process(3, "b", 1.0)];
        procs[0].gpu = 99.0;
        procs[0].user = String::new();
        procs[1].gpu = 5.0;
        assert_eq!(
            order(&procs, ProcSort::Gpu, false),
            [1, 2, 3],
            "WindowServer tops gpu%"
        );
        assert_eq!(order(&procs, ProcSort::Gpu, true), [3, 2, 1]);
        for reversed in [false, true] {
            assert_eq!(order(&procs, ProcSort::User, reversed).last(), Some(&1));
            assert_eq!(order(&procs, ProcSort::Started, reversed).len(), 3);
        }
    }

    fn tree_fixture() -> Vec<Process> {
        // 1 ─┬─ 2 ─┬─ 4
        //    │     ╰─ 5 ── 7 ── 8 ── 9 ── 10 ── 11
        //    ╰─ 3 ── 6
        let edges = [
            (1, 0),
            (2, 1),
            (3, 1),
            (4, 2),
            (5, 2),
            (6, 3),
            (7, 5),
            (8, 7),
            (9, 8),
            (10, 9),
            (11, 10),
        ];
        edges
            .iter()
            .map(|&(pid, ppid)| Process {
                ppid,
                ..process(pid, &format!("p{pid}"), 0.0)
            })
            .collect()
    }

    #[test]
    fn tree_guides_draw_rails_for_open_ancestors() {
        let procs = tree_fixture();
        let model = ProcModel::build(&procs, "", ProcSort::Pid, false, true);
        let lines: Vec<String> = model
            .rows()
            .iter()
            .map(|r| format!("{}p{}", r.guide.prefix(r.depth), r.pid))
            .collect();
        assert_eq!(
            lines,
            [
                "p1",
                "├ p2",
                "│ ├ p4",
                "│ ╰ p5",
                "│   ╰ p7",
                "│     ╰ p8",
                "│       ╰ p9",
                "│         ╰ p10",
                "│         ╰ p11",
                "╰ p3",
                "  ╰ p6",
            ]
        );
    }

    #[test]
    fn tree_indent_is_capped() {
        let procs = tree_fixture();
        let model = ProcModel::build(&procs, "", ProcSort::Pid, false, true);
        let widest = model
            .rows()
            .iter()
            .map(|r| r.guide.prefix(r.depth).chars().count())
            .max()
            .unwrap();
        assert_eq!(widest, usize::from(TREE_INDENT_MAX) * 2);
        assert_eq!(Guide::default().prefix(0), "");
    }

    #[test]
    fn filtered_tree_marks_the_last_visible_sibling() {
        let procs = tree_fixture();
        let model = ProcModel::build(&procs, "p4", ProcSort::Pid, false, true);
        let lines: Vec<String> = model
            .rows()
            .iter()
            .map(|r| format!("{}p{}", r.guide.prefix(r.depth), r.pid))
            .collect();
        assert_eq!(
            lines,
            ["p1", "╰ p2", "  ╰ p4"],
            "p3 and p5 are filtered out"
        );
    }

    #[test]
    fn tree_nests_and_filters_keep_ancestors() {
        let mut procs = vec![
            process(10, "parent", 1.0),
            process(11, "child", 9.0),
            process(12, "other", 5.0),
        ];
        procs[1].ppid = 10;
        let model = ProcModel::build(&procs, "", ProcSort::Cpu, false, true);
        let rows: Vec<(u8, u32)> = model.rows().iter().map(|r| (r.depth, r.pid)).collect();
        assert_eq!(rows, [(0, 12), (0, 10), (1, 11)]);
        let model = ProcModel::build(&procs, "child", ProcSort::Cpu, false, true);
        let rows: Vec<u32> = model.rows().iter().map(|r| r.pid).collect();
        assert_eq!(rows, [10, 11]);
    }

    #[test]
    fn filter_matches_name_command_and_pid() {
        let mut procs = vec![process(904, "Xcode", 1.0), process(1234, "Finder", 2.0)];
        procs[1].command = Some(String::from("/System/Finder.app"));
        let pick = |needle: &str| -> Vec<u32> {
            ProcModel::build(&procs, needle, ProcSort::Cpu, false, false)
                .rows()
                .iter()
                .map(|r| r.pid)
                .collect()
        };
        assert_eq!(pick("904"), [904]);
        assert_eq!(pick("xcode"), [904]);
        assert_eq!(pick("system"), [1234]);
        assert!(pick("zzz").is_empty());
    }

    // ── app groups, folds and tags ──────────────────────────────────────

    /// A realistic slice of `ps`: Safari with two nested helpers, Slack
    /// with one, a lone bundled app, and two bare daemons.
    fn app_fixture() -> Vec<Process> {
        let row = |pid: u32, name: &str, cmd: &str, cpu: f32, mem: u64| Process {
            command: Some(cmd.to_owned()),
            mem_bytes: Some(mem),
            energy_w: Some(cpu / 10.0),
            gpu: cpu / 2.0,
            ..process(pid, name, cpu)
        };
        vec![
            row(
                10,
                "Safari",
                "/Applications/Safari.app/Contents/MacOS/Safari",
                5.0,
                500,
            ),
            row(
                11,
                "Safari Web Content",
                "/Applications/Safari.app/Contents/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent",
                20.0,
                900,
            ),
            row(
                12,
                "Safari Networking",
                "/Applications/Safari.app/Contents/Frameworks/Net Helper.app/Contents/MacOS/Net Helper",
                1.0,
                50,
            ),
            row(
                20,
                "Slack",
                "/Applications/Slack.app/Contents/MacOS/Slack",
                2.0,
                300,
            ),
            row(
                21,
                "Slack Helper (Renderer)",
                "/Applications/Slack.app/Contents/Frameworks/Slack Helper (Renderer).app/Contents/MacOS/Slack Helper (Renderer)",
                3.0,
                400,
            ),
            row(
                30,
                "Notes",
                "/System/Applications/Notes.app/Contents/MacOS/Notes",
                0.5,
                80,
            ),
            row(40, "launchd", "/sbin/launchd", 0.1, 20),
            row(41, "rustc", "/Users/me/.rustup/bin/rustc", 30.0, 1000),
        ]
    }

    fn grouped(procs: &[Process], sort: ProcSort, open: &[&str]) -> ProcModel {
        let mut folds = Folds::default();
        for bundle in open {
            folds.open_apps.set((*bundle).to_owned(), true);
        }
        ProcModel::build_nested(procs, "", sort, false, Nesting::Apps, &folds)
    }

    fn pids(model: &ProcModel) -> Vec<u32> {
        model.rows().iter().map(|r| r.pid).collect()
    }

    #[test]
    fn app_bundle_is_the_outermost_app_in_the_path() {
        let procs = app_fixture();
        let bundle = |i: usize| app_bundle(&procs[i]);
        assert_eq!(bundle(0), Some("/Applications/Safari.app"));
        assert_eq!(bundle(1), Some("/Applications/Safari.app"), "xpc helper");
        assert_eq!(bundle(2), Some("/Applications/Safari.app"), "nested .app");
        assert_eq!(bundle(4), Some("/Applications/Slack.app"));
        assert_eq!(bundle(6), None);
        assert_eq!(bundle(7), None);
    }

    #[test]
    fn python_scripts_are_not_grouped_under_the_interpreter_bundle() {
        // Seen live: every python script collapsed into one "soak.py +4".
        let script = |pid: u32, name: &str| Process {
            command: Some(String::from(
                "/Library/Frameworks/Python.framework/Versions/3.12/Resources/Python.app/Contents/MacOS/Python",
            )),
            ..process(pid, name, 1.0)
        };
        let procs = vec![script(1, "soak.py"), script(2, "shot.py")];
        assert_eq!(app_bundle(&procs[0]), None);
        let model = grouped(&procs, ProcSort::Pid, &[]);
        assert_eq!(pids(&model), [1, 2], "two separate programs");
        assert!(model.rows().iter().all(|r| r.agg.is_none()));
    }

    #[test]
    fn xcode_toolchain_runs_are_not_grouped_under_xcode() {
        // Seen live: python scripts and git runs from Xcode's developer
        // dir folded into one "http.server +4" row that x signalled whole.
        let run = |pid: u32, name: &str, command: &str| Process {
            command: Some(String::from(command)),
            ..process(pid, name, 1.0)
        };
        let dev = "/Applications/Xcode.app/Contents/Developer";
        let python = format!(
            "{dev}/Library/Frameworks/Python3.framework/Versions/3.9/Resources/Python.app/Contents/MacOS/Python"
        );
        let procs = vec![
            run(1, "http.server", &python),
            run(2, "shot.py", &python),
            run(3, "git", &format!("{dev}/usr/bin/git")),
            run(
                4,
                "clang",
                &format!("{dev}/Toolchains/XcodeDefault.xctoolchain/usr/bin/clang"),
            ),
            run(5, "Xcode", "/Applications/Xcode.app/Contents/MacOS/Xcode"),
            run(
                6,
                "Simulator",
                &format!("{dev}/Applications/Simulator.app/Contents/MacOS/Simulator"),
            ),
        ];
        for proc in &procs[..4] {
            assert_eq!(app_bundle(proc), None, "{}", proc.name);
        }
        assert_eq!(app_bundle(&procs[4]), Some("/Applications/Xcode.app"));
        assert_eq!(
            app_bundle(&procs[5]),
            Some("/Applications/Xcode.app/Contents/Developer/Applications/Simulator.app"),
        );
        let model = grouped(&procs, ProcSort::Pid, &[]);
        assert_eq!(pids(&model), [1, 2, 3, 4, 5, 6], "six separate programs");
        assert!(model.rows().iter().all(|r| r.agg.is_none()));
    }

    #[test]
    fn app_bundle_falls_back_to_argv_and_ignores_bare_names() {
        let mut proc = process(1, "x", 0.0);
        proc.command = Some(String::from("x"));
        proc.args = vec![String::from("/Applications/Zed.app/Contents/MacOS/zed")];
        assert_eq!(app_bundle(&proc), Some("/Applications/Zed.app"));
        proc.args.clear();
        assert_eq!(app_bundle(&proc), None);
        // ".app" without a slash after it is a file name, not a bundle
        proc.command = Some(String::from("/tmp/my.app"));
        assert_eq!(app_bundle(&proc), None);
    }

    #[test]
    fn helpers_collapse_into_their_app() {
        let procs = app_fixture();
        let model = grouped(&procs, ProcSort::Pid, &[]);
        assert_eq!(pids(&model), [10, 20, 30, 40, 41]);
        let safari = &model.rows()[0];
        assert_eq!(safari.fold, Fold::Closed);
        assert_eq!(safari.agg.map(|a| a.members), Some(3));
        let notes = &model.rows()[2];
        assert_eq!(
            notes.fold,
            Fold::Leaf,
            "a lone bundled process is a plain row"
        );
        assert!(notes.agg.is_none());
    }

    #[test]
    fn group_row_sums_cpu_mem_energy_and_gpu() {
        let procs = app_fixture();
        let model = grouped(&procs, ProcSort::Pid, &[]);
        let agg = model.rows()[0].agg.unwrap();
        assert!((agg.cpu.unwrap() - 26.0).abs() < 1e-4);
        assert_eq!(agg.mem, Some(1450));
        assert!((agg.energy.unwrap() - 2.6).abs() < 1e-4);
        assert!((agg.gpu - 13.0).abs() < 1e-4);
        assert_eq!(agg.threads, Some(3));
        let shown = model.rows()[0].shown(&procs[0]);
        assert_eq!(shown.mem_bytes, Some(1450));
        assert_eq!(shown.name, "Safari", "the head names the group");
    }

    #[test]
    fn group_sums_skip_unknown_members_and_never_invent_zero() {
        let mut procs = app_fixture();
        for p in &mut procs[..3] {
            p.cpu = None;
            p.mem_bytes = None;
            p.energy_w = None;
            p.restricted = true;
        }
        procs[1].cpu = Some(7.0);
        let agg = grouped(&procs, ProcSort::Pid, &[]).rows()[0].agg.unwrap();
        assert_eq!(agg.cpu, Some(7.0));
        assert_eq!(agg.mem, None, "all unknown stays unknown");
        assert_eq!(agg.energy, None);
        let gpu: f32 = procs[..3].iter().map(|p| p.gpu).sum();
        assert!((agg.gpu - gpu).abs() < 1e-4, "restricted gpu is readable");
    }

    #[test]
    fn sort_orders_groups_by_their_sums() {
        let procs = app_fixture();
        // rustc 30 > Safari 26 (summed) > Slack 5
        let model = grouped(&procs, ProcSort::Cpu, &[]);
        assert_eq!(pids(&model), [41, 10, 20, 30, 40]);
        // by memory Safari's 1450 beats rustc's 1000, though no single
        // Safari process does
        let model = grouped(&procs, ProcSort::Mem, &[]);
        assert_eq!(pids(&model), [10, 41, 20, 30, 40]);
    }

    #[test]
    fn open_group_lists_helpers_sorted_under_the_head() {
        let procs = app_fixture();
        let model = grouped(&procs, ProcSort::Cpu, &["/Applications/Safari.app"]);
        assert_eq!(pids(&model), [41, 10, 11, 12, 20, 30, 40]);
        let head = model.rows()[1];
        assert_eq!(head.fold, Fold::Open);
        assert!(head.agg.is_some(), "an open group still shows its sums");
        let lines: Vec<String> = model.rows()[1..4]
            .iter()
            .map(|r| format!("{}{}", r.guide.prefix(r.depth), r.pid))
            .collect();
        assert_eq!(lines, ["10", "├ 11", "╰ 12"]);
    }

    #[test]
    fn members_of_a_group_are_every_pid_and_a_plain_row_is_itself() {
        let procs = app_fixture();
        let model = grouped(&procs, ProcSort::Pid, &[]);
        assert_eq!(model.members_of(10), [10, 11, 12]);
        assert_eq!(model.members_of(20), [20, 21]);
        assert_eq!(model.members_of(41), [41]);
    }

    #[test]
    fn grouping_filters_members_before_summing() {
        let procs = app_fixture();
        let model = ProcModel::build_nested(
            &procs,
            "web",
            ProcSort::Pid,
            false,
            Nesting::Apps,
            &Folds::default(),
        );
        assert_eq!(pids(&model), [11], "one matching helper is a plain row");
    }

    #[test]
    fn group_open_state_survives_refresh_and_head_restart() {
        let mut procs = app_fixture();
        let mut view = ProcView {
            grouped: true,
            ..ProcView::default()
        };
        view.sync(&procs, ProcSort::Cpu, false, false);
        view.select(1, 20);
        assert!(view.fold_selected(Some(true), &procs));
        view.sync(&procs, ProcSort::Cpu, false, false);
        assert_eq!(view.model.len(), 7);
        for round in 0..5_u32 {
            // helpers churn: a new pid replaces a helper each sample
            procs[1].pid = 100 + round;
            procs[1].cpu = Some(round as f32);
            view.generation += 1;
            view.sync(&procs, ProcSort::Cpu, false, false);
            assert_eq!(view.model.len(), 7, "round {round}: still open");
        }
    }

    #[test]
    fn enter_toggles_a_group_and_a_leaf_has_nothing_to_fold() {
        let procs = app_fixture();
        let mut view = ProcView {
            grouped: true,
            ..ProcView::default()
        };
        view.sync(&procs, ProcSort::Pid, false, false);
        view.select(3, 20);
        assert!(!view.fold_selected(None, &procs), "launchd is a leaf");
        view.select(0, 20);
        assert!(view.fold_selected(None, &procs));
        view.sync(&procs, ProcSort::Pid, false, false);
        assert_eq!(view.model.len(), 7);
        assert!(!view.fold_selected(Some(true), &procs), "already open");
        assert!(view.fold_selected(None, &procs));
        view.sync(&procs, ProcSort::Pid, false, false);
        assert_eq!(view.model.len(), 5);
    }

    #[test]
    fn tree_wins_over_grouping() {
        let procs = app_fixture();
        let view = ProcView {
            grouped: true,
            ..ProcView::default()
        };
        assert_eq!(view.nesting(true), Nesting::Tree);
        assert_eq!(view.nesting(false), Nesting::Apps);
        assert_eq!(ProcView::default().nesting(false), Nesting::Flat);
        let model = view.current(&procs, ProcSort::Pid, false, true);
        assert!(model.rows().iter().all(|r| r.agg.is_none()));
    }

    fn tree_view(procs: &[Process]) -> ProcView {
        let mut view = ProcView::default();
        view.sync(procs, ProcSort::Pid, false, true);
        view
    }

    #[test]
    fn collapsed_subtree_shows_its_totals() {
        let mut procs = tree_fixture();
        for p in &mut procs {
            p.cpu = Some(p.pid as f32);
            p.mem_bytes = Some(u64::from(p.pid) * 10);
        }
        let mut view = tree_view(&procs);
        // select p5 (row 3) and collapse it: 5, 7, 8, 9, 10, 11
        view.select(3, 40);
        assert!(view.fold_selected(Some(false), &procs));
        view.sync(&procs, ProcSort::Pid, false, true);
        assert_eq!(pids(&view.model), [1, 2, 4, 5, 3, 6]);
        let row = view.model.rows()[3];
        assert_eq!(row.fold, Fold::Closed);
        let agg = row.agg.unwrap();
        assert_eq!(agg.members, 6);
        assert!((agg.cpu.unwrap() - 50.0).abs() < 1e-4);
        assert_eq!(agg.mem, Some(500));
        assert_eq!(view.model.rows()[1].fold, Fold::Open, "p2 still open");
        assert!(
            view.model.rows()[1].agg.is_none(),
            "open nodes show their own values"
        );
        assert_eq!(view.model.rows()[2].fold, Fold::Leaf);
    }

    #[test]
    fn collapse_state_is_stable_across_refresh_and_resort() {
        let mut procs = tree_fixture();
        let mut view = tree_view(&procs);
        view.select(1, 40); // p2
        view.fold_selected(Some(false), &procs);
        for round in 0..6_u32 {
            for p in &mut procs {
                p.cpu = Some(((p.pid * 7 + round * 3) % 11) as f32);
            }
            view.generation += 1;
            let sort = if round % 2 == 0 {
                ProcSort::Cpu
            } else {
                ProcSort::Pid
            };
            view.sync(&procs, sort, round % 3 == 0, true);
            let listed = pids(&view.model);
            assert!(listed.contains(&2), "round {round}");
            for hidden in [4, 5, 7, 8] {
                assert!(!listed.contains(&hidden), "round {round}: {hidden} hidden");
            }
            assert_eq!(
                view.model.len(),
                4,
                "round {round}: 1 2 3 6 and nothing else"
            );
        }
    }

    #[test]
    fn expand_all_and_collapse_all() {
        let procs = tree_fixture();
        let mut view = tree_view(&procs);
        view.fold_all(false);
        view.sync(&procs, ProcSort::Pid, false, true);
        assert_eq!(pids(&view.model), [1], "one root, everything under it");
        assert_eq!(view.model.rows()[0].agg.map(|a| a.members), Some(11));
        view.select(0, 40);
        view.fold_selected(Some(true), &procs);
        view.sync(&procs, ProcSort::Pid, false, true);
        assert_eq!(pids(&view.model), [1, 2, 3], "children arrive collapsed");
        view.fold_all(true);
        view.sync(&procs, ProcSort::Pid, false, true);
        assert_eq!(view.model.len(), 11);
    }

    #[test]
    fn fold_set_sweep_matches_a_reference_set() {
        let mut seed = 0x9e37_79b9_u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let mut set = FoldSet::<u32>::default();
        let mut reference = std::collections::HashSet::new();
        let mut all = false;
        for _ in 0..500 {
            let key = next() % 16;
            match next() % 5 {
                0 => {
                    set.toggle(key);
                    if !reference.remove(&key) {
                        reference.insert(key);
                    }
                }
                1 | 2 => {
                    let on = next() % 2 == 0;
                    set.set(key, on);
                    if on == all {
                        reference.remove(&key);
                    } else {
                        reference.insert(key);
                    }
                }
                3 => {
                    all = next() % 2 == 0;
                    set.fill(all);
                    reference.clear();
                }
                _ => {}
            }
            for k in 0..16 {
                assert_eq!(set.contains(&k), all != reference.contains(&k), "key {k}");
            }
        }
    }

    #[test]
    fn tags_survive_resort() {
        let mut procs = many(8);
        let mut view = ProcView::default();
        view.sync(&procs, ProcSort::Cpu, false, false);
        view.toggle_tags(&[3], &procs);
        view.toggle_tags(&[6], &procs);
        for sort in [ProcSort::Mem, ProcSort::Pid, ProcSort::Name] {
            procs.reverse();
            view.generation += 1;
            view.sync(&procs, sort, true, false);
            view.prune(&procs);
            assert_eq!(view.tags.pids().collect::<Vec<_>>(), [3, 6]);
        }
    }

    #[test]
    fn tags_drop_when_a_pid_exits() {
        let mut procs = many(5);
        let mut view = ProcView::default();
        view.toggle_tags(&[2, 4], &procs);
        view.folds.closed.set(4, true);
        procs.retain(|p| p.pid != 4);
        view.prune(&procs);
        assert_eq!(view.tags.pids().collect::<Vec<_>>(), [2]);
        assert!(
            !view.folds.closed.contains(&4),
            "a dead pid's fold is forgotten"
        );
    }

    #[test]
    fn tags_drop_when_a_new_process_reuses_the_pid() {
        // A long freeze can span a pid wrap: the first sample after it
        // must not hand the tag (and the next x) to a stranger.
        let mut procs = many(5);
        let mut view = ProcView::default();
        view.toggle_tags(&[2, 4], &procs);
        procs[3].start_unix += 3_600;
        procs[3].name = String::from("unrelated");
        view.prune(&procs);
        assert_eq!(view.tags.pids().collect::<Vec<_>>(), [2]);
        assert!(!view.tags.contains(4));
    }

    #[test]
    fn tagging_skips_pids_not_on_screen() {
        let procs = many(3);
        let mut view = ProcView::default();
        view.toggle_tags(&[2, 9], &procs);
        assert_eq!(view.tags.pids().collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn tagging_a_group_tags_every_member_and_again_untags() {
        let procs = many(12);
        let mut view = ProcView::default();
        view.toggle_tags(&[11], &procs);
        view.toggle_tags(&[10, 11, 12], &procs);
        assert_eq!(view.tags.len(), 3, "partly tagged group tags the rest");
        view.toggle_tags(&[10, 11, 12], &procs);
        assert!(view.tags.is_empty());
    }
}
