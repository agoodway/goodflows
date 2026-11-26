//! Change requests: `goodflows cr`, `policy`, `features sync` and `events`.
//!
//! The goodflows server stores change requests, the facts and claims
//! reported for each head, scenario sets, verify runs and the project's
//! review policy, and computes the verdict. These commands report those
//! facts from the local checkout and read the results back. They use the
//! `[reporting]` project, URL and key, like `history`.

mod api;
mod changes;
mod git;
pub(crate) mod markdown;
pub(crate) mod verify_run;

use crate::config::Reporting;
use crate::output::Output;
use anyhow::{anyhow, bail, Context, Result};
use api::{data, explain_conflict, quoted, Api, Body, HttpError};
use changes::dependencies_at;
pub use changes::{change, ChangeCommand};
use clap::{Args, Subcommand, ValueEnum};
use git::Git;
use markdown::Problems;
use reqwest::Method;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::watch;

/// Exit code of `cr wait` when its timeout passes first, as for `wait`.
const TIMED_OUT: u8 = 75;

/// The server's `409` details for a different report at the same head.
const GIT_FACTS_CONFLICT: &str = "Different Git facts are stored for this head and target";
const POLICY_CONFLICT: &str = "previous_version is not the current policy version";
const CLAIMS_CONFLICT: &str = "Different claims are stored for this head";

/// Retries of `POST verify-runs/:run_id/commit` answered `503`.
const COMMIT_RETRIES: u32 = 5;

pub const CR_EXAMPLES: &str = "A change request's description is its claims file (change-claims/2, written by gf-change-request):
  CREATE with cr open --claims <file> while the file's cr is null; it writes the new number into cr.
  UPDATE with cr claims <file> once cr is set, after each new commit.
Commands find the change request of the current branch unless given --number. They talk to the
GoodFlows server of the config's [reporting] project, at $GOODFLOWS_URL with the key in
$GOODFLOWS_API_KEY (the block's url_env and token_env); writes need an sk_ key. --json prints one
JSON event. Failures exit 1 and bad arguments 2; cr wait exits 75 when its timeout passes.

Examples:
  goodflows cr open                           # create from the default claims file (goodflows cr path)
  goodflows cr claims                         # update its claims for HEAD after a new commit
  goodflows cr verdict                        # pending, blocked, needs_review or mergeable, with reasons";

#[derive(Args, Debug, Clone, Copy, Default)]
pub struct Pick {
    /// The change request number (default: the one draft or open change request of the current branch)
    #[arg(short, long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    number: Option<u64>,
}

#[derive(Subcommand, Debug)]
pub enum CrCommand {
    /// CREATE a change request from a claims (description) file whose cr is null, or with --title before the file exists
    #[command(
        after_help = "Finds the one draft or open change request whose source branch is the current branch, or
creates a draft for it. When the claims file exists, it also reports Git facts for HEAD, writes the
change request number into the file's cr, and uploads the claims; the file's head must be HEAD.
Without the file it prints the number, so an intent scenario set can be uploaded and approved
before code. Once cr is set, update the change request with cr claims instead.

Examples:
  goodflows cr open                           # create from the default claims file (goodflows cr path)
  goodflows cr open --claims cr.md --json     # a claims file elsewhere; print the result as JSON
  goodflows cr open --change add-login --title Login   # before code, without claims"
    )]
    Open {
        /// The change-claims/2 file, which must exist (default: the path cr path prints, used only when it exists)
        #[arg(long, value_name = "FILE")]
        claims: Option<PathBuf>,
        /// The id of the goodflows change (goodflows/changes/<id>/) this change request implements (a-z, 0-9 and -); set only when it is created, and it cannot change
        #[arg(long = "change", value_name = "ID", value_parser = change_id)]
        change_id: Option<String>,
        /// Title of a new change request (default: the claims file's # heading; required without a claims file)
        #[arg(long)]
        title: Option<String>,
        /// Target branch of a new change request (default: the project's default branch on the server); to change it later, use cr retarget
        #[arg(long, value_name = "BRANCH")]
        target: Option<String>,
        /// Skip git fetch origin <target> and use the target branch as this checkout has it
        #[arg(long)]
        no_fetch: bool,
    },
    /// UPDATE a change request's description: report Git facts for HEAD, then upload the claims file
    #[command(
        mut_arg("number", |arg| arg.help("The change request number, which must match the file's cr (default: the file's cr, else the one draft or open change request of the current branch)")),
        after_help = "The file's cr names the change request; when it is null, the change request of the current
branch is used and its number written into cr. Claims describe one head: commit first, set the
file's head to HEAD, and upload new claims after each new commit. A change request that does not
exist yet is created with cr open.

Examples:
  goodflows cr claims                         # the default claims file (goodflows cr path)
  goodflows cr claims cr.md                   # a claims file elsewhere
  goodflows cr claims --json                  # print the result as a JSON event"
    )]
    Claims {
        /// The change-claims/2 file (default: .goodflows/change-requests/<branch>/cr.md)
        file: Option<PathBuf>,
        #[command(flatten)]
        pick: Pick,
        /// Skip git fetch origin <target> and use the target branch as this checkout has it
        #[arg(long)]
        no_fetch: bool,
    },
    /// Run git push, then report Git facts for the new head (upload its claims with cr claims)
    #[command(after_help = "Examples:
  goodflows cr push                           # git push, then report Git facts for HEAD
  goodflows cr push -- -u origin HEAD         # arguments after -- go to git push
  goodflows cr push --json                    # print the facts and the verdict as JSON")]
    Push {
        #[command(flatten)]
        pick: Pick,
        /// Skip git fetch origin <target> and use the target branch as this checkout has it
        #[arg(long)]
        no_fetch: bool,
        /// Arguments for git push, after --, such as -u origin HEAD (default: none)
        #[arg(last = true, value_name = "GIT_PUSH_ARGS")]
        git_args: Vec<String>,
    },
    /// Show a change request with its verdict and state history
    #[command(after_help = "Examples:
  goodflows cr status                         # the current branch's change request
  goodflows cr status -n 42                   # change request 42
  goodflows cr status --json                  # the server's record as JSON")]
    Status {
        #[command(flatten)]
        pick: Pick,
    },
    /// Print the default claims file path for the current branch (needs no server)
    #[command(after_help = "Examples:
  goodflows cr path                           # .goodflows/change-requests/<branch>/cr.md, absolute
  goodflows cr path --json                    # with the branch and the default diff set path (qa-scenarios.md)
  goodflows cr open                           # then create the change request from it")]
    Path,
    /// Check claims files, scenario sets and run folders as the commands that upload them read them, and list every problem
    #[command(
        after_help = "Each file is checked by the format in its front matter: change-claims/2 as cr open and cr claims
read it, qa-scenarios/2 as cr scenarios upload reads it, and a .verify/<app>/<run-id>/ folder (or
its run.md) as cr runs upload reads it. Every problem in a file is listed; warnings name what an
upload would trip on later, such as claims for a head other than HEAD. Exits 1 when any file has
errors. Needs no API key or goodflows.toml. The server checks more of a claims file than its
format, cr and head; of the rest, missing and unknown keys, door, proof, ran facts without evidence and
recommendations outside their options are warnings. Feature files: goodflows features validate.
How to write each format: goodflows schema show <format>.

Without paths it checks the branch's claims file and diff set (goodflows cr path), each
goodflows/changes/*/qa-scenarios.md and each .verify/<app>/<run-id>/ run folder.

Examples:
  goodflows cr validate                       # every change request file in the checkout
  goodflows cr validate .goodflows/change-requests/feat/login/cr.md   # one file
  goodflows cr validate .verify/web/20261119T143000Z-a1b2c3d   # a run folder
  goodflows cr validate --json                # every file's errors and warnings as JSON"
    )]
    Validate {
        /// Files or run folders to check (default: the change request files in the checkout)
        paths: Vec<PathBuf>,
    },
    /// Show the verdict: pending, blocked, needs_review or mergeable, with one reason per rule that fired
    #[command(after_help = "Examples:
  goodflows cr verdict                        # the current branch's change request
  goodflows cr verdict -n 42 --json           # change request 42 as JSON
  goodflows cr wait --until mergeable         # wait for a state instead")]
    Verdict {
        #[command(flatten)]
        pick: Pick,
    },
    /// Wait until the verdict reaches one of the given states (exit 75 when the timeout passes first)
    #[command(after_help = "Examples:
  goodflows cr wait --until mergeable,needs_review        # wait for a person or a merge
  goodflows cr wait --until mergeable --timeout 600       # give up after 10 minutes; exit 75
  goodflows cr wait --until blocked,mergeable --interval 30   # check every 30 seconds")]
    Wait {
        #[command(flatten)]
        pick: Pick,
        /// Verdict states to wait for, comma-separated or repeated; exits 0 once the verdict is one of them
        #[arg(long, value_enum, value_delimiter = ',', required = true)]
        until: Vec<VerdictState>,
        /// Stop waiting after this many seconds and exit 75 (default: no limit)
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,
        /// Seconds between checks, 1-3600
        #[arg(long, value_name = "SECS", default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=3600))]
        interval: u64,
    },
    /// Move a draft change request to open
    #[command(after_help = "Examples:
  goodflows cr ready                          # the current branch's change request
  goodflows cr ready -n 42                    # change request 42
  goodflows cr draft                          # move it back to draft")]
    Ready {
        #[command(flatten)]
        pick: Pick,
    },
    /// Move an open change request back to draft
    #[command(after_help = "Examples:
  goodflows cr draft                          # the current branch's change request
  goodflows cr draft -n 42                    # change request 42
  goodflows cr ready                          # move it to open again")]
    Draft {
        #[command(flatten)]
        pick: Pick,
    },
    /// Close a draft or open change request
    #[command(after_help = "Examples:
  goodflows cr close                          # the current branch's change request
  goodflows cr close -n 42                    # change request 42
  goodflows cr reopen -n 42                   # reopen it in the state it had")]
    Close {
        #[command(flatten)]
        pick: Pick,
    },
    /// Reopen a closed change request in the state it had (default: the one closed change request of the current branch)
    #[command(
        mut_arg("number", |arg| arg.help("The change request number (default: the one closed change request of the current branch)")),
        after_help = "Examples:
  goodflows cr reopen -n 42                   # change request 42
  goodflows cr reopen --json                  # print the change request as JSON
  goodflows cr close -n 42                    # close it again"
    )]
    Reopen {
        #[command(flatten)]
        pick: Pick,
    },
    /// Upload, list, approve, reject and compare QA scenario sets
    #[command(after_help = "Examples:
  goodflows cr scenarios upload               # the branch's diff set, .goodflows/change-requests/<branch>/qa-scenarios.md
  goodflows cr scenarios list                 # every version, approved or accepted
  goodflows cr scenarios approve              # approve the current intent set
  goodflows cr scenarios diff                 # what changed since the approved version")]
    Scenarios {
        #[command(subcommand)]
        command: ScenariosCommand,
    },
    /// Upload verify runs that graded a scenario set, with their evidence
    #[command(after_help = "Examples:
  goodflows cr runs upload .verify/web/20261119T143000Z-a1b2c3d   # upload a run folder
  goodflows cr runs upload .verify/web/20261119T143000Z-a1b2c3d --json   # print the result as JSON
  goodflows cr verdict                        # then read the verdict")]
    Runs {
        #[command(subcommand)]
        command: RunsCommand,
    },
    /// Show the review packet: what the policy flagged for a person, with the answers each item allows
    #[command(after_help = "Examples:
  goodflows cr review                         # Markdown with images and answer commands
  goodflows cr review -n 42 --json            # the packet data as JSON
  goodflows cr answer question:9c1b choose --option b   # then answer an item")]
    Review {
        #[command(flatten)]
        pick: Pick,
    },
    /// Answer a review packet item from cr review: approve, reject, request_change or choose, at the current head
    #[command(after_help = "Examples:
  goodflows cr answer one_way_door:3f2a approve                 # approve an item
  goodflows cr answer question:9c1b choose --option b           # choose an option
  goodflows cr answer path_match:77de request_change --note Add-a-down-migration   # ask for a change")]
    Answer {
        #[command(flatten)]
        pick: Pick,
        /// The item id from cr review, such as question:9c1b
        item_id: String,
        /// The answer; cr review lists the answers each item allows
        #[arg(value_enum)]
        answer: AnswerKind,
        /// The chosen option (required for choose, and only for choose)
        #[arg(long)]
        option: Option<String>,
        /// Why (required for reject and request_change)
        #[arg(long)]
        note: Option<String>,
        /// The head the answer is for (default: the change request's current head)
        #[arg(long, value_name = "SHA")]
        head: Option<String>,
    },
    /// List the change request's tasks, or mark one done with cr tasks done <TASK_ID>
    #[command(
        // Global, so `-n` goes before or after `done`.
        mut_arg("number", |arg| arg.global(true)),
        after_help = "Examples:
  goodflows cr tasks                          # open and stale tasks
  goodflows cr tasks --state done             # finished tasks
  goodflows cr tasks done 5b1f                # mark task 5b1f done"
    )]
    Tasks {
        #[command(flatten)]
        pick: Pick,
        /// Only tasks in this state (default: open and stale; not with done)
        #[arg(long, value_enum)]
        state: Option<TaskState>,
        #[command(subcommand)]
        command: Option<TasksCommand>,
    },
    /// Report that the change request was merged on the Git host (goodflows never merges)
    #[command(after_help = "Examples:
  goodflows cr merged --commit HEAD           # the merge commit checked out now
  goodflows cr merged -n 42 --commit 3f2a9c1 --target main   # a merge into main
  goodflows cr merged --commit HEAD --lease 7c0e   # name the merge lease held")]
    Merged {
        #[command(flatten)]
        pick: Pick,
        /// The merged commit on the target branch: a full SHA, or a Git revision this checkout resolves
        #[arg(long, value_name = "REV")]
        commit: String,
        /// The branch it was merged into (default: the change request's target branch)
        #[arg(long, value_name = "BRANCH")]
        target: Option<String>,
        /// The id of the merge lease held for the merge, from cr lease acquire
        #[arg(long, value_name = "LEASE_ID")]
        lease: Option<String>,
    },
    /// Change the target branch on the server (no Git ref changes); then report Git facts again with cr claims
    #[command(after_help = "Examples:
  goodflows cr retarget main                  # target main after the parent merged
  goodflows cr retarget release -n 42         # change request 42
  goodflows cr claims                         # then report facts and claims again")]
    Retarget {
        #[command(flatten)]
        pick: Pick,
        /// The new target branch
        branch: String,
    },
    /// Set or clear the parent change request of a stacked change
    #[command(after_help = "Examples:
  goodflows cr parent 41                      # stack on change request 41
  goodflows cr parent --clear                 # remove the parent
  goodflows cr parent 41 -n 42                # make 41 the parent of 42")]
    Parent {
        #[command(flatten)]
        pick: Pick,
        /// The parent change request's number (required unless --clear)
        #[arg(required_unless_present = "clear", value_parser = clap::value_parser!(u64).range(1..))]
        parent: Option<u64>,
        /// Remove the parent (conflicts with PARENT)
        #[arg(long, conflicts_with = "parent")]
        clear: bool,
    },
    /// Acquire, renew or release the merge slot lease of the target branch
    #[command(after_help = "Examples:
  goodflows cr lease acquire                  # take the slot for HEAD
  goodflows cr lease heartbeat 7c0e           # extend lease 7c0e
  goodflows cr lease release 7c0e             # free the slot")]
    Lease {
        #[command(subcommand)]
        command: LeaseCommand,
    },
    /// Record a defect found after a merge, by change request number or commit
    #[command(after_help = "Examples:
  goodflows cr defect --commit 3f2a9c1 --ticket GOO-400 --severity high   # by commit
  goodflows cr defect --number 42 --severity low   # by change request
  goodflows policy calibration                # then see defects by lane and rule")]
    Defect {
        /// The change request that shipped the defect (conflicts with --commit; one of the two is required)
        #[arg(short, long, value_name = "N", required_unless_present = "commit", conflicts_with = "commit", value_parser = clap::value_parser!(u64).range(1..))]
        number: Option<u64>,
        /// A commit that shipped the defect: a Git revision this checkout resolves (conflicts with --number)
        #[arg(long, value_name = "REV")]
        commit: Option<String>,
        /// The ticket that tracks the defect, such as GOO-400
        #[arg(long, value_name = "ID")]
        ticket: Option<String>,
        /// How bad the defect is (default: none recorded)
        #[arg(long, value_enum)]
        severity: Option<Severity>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ScenariosCommand {
    /// Upload a qa-scenarios/2 set file exactly as written, as a new version of its intent or diff set
    #[command(
        after_help = "The set's digest covers its raw bytes, so the file is never reformatted; any edit needs a new
approval. Each scenario's section digest covers its heading under ## Scenarios (### 3. Name, or
#### 3. Name under a group heading) up to the next heading at its level or higher. A diff set
whose revision ends +dirty is refused. Results never go into the set: upload them with cr runs upload.

Examples:
  goodflows cr scenarios upload               # the default diff set (goodflows cr path --json)
  goodflows cr scenarios upload goodflows/changes/add-login/qa-scenarios.md   # an intent set
  goodflows cr scenarios upload qa.md -n 42   # change request 42
  goodflows cr scenarios upload qa.md --json  # print the version and digest as JSON"
    )]
    Upload {
        /// The qa-scenarios/2 file; its front matter's source (intent or diff) names the set (default: .goodflows/change-requests/<branch>/qa-scenarios.md)
        file: Option<PathBuf>,
        #[command(flatten)]
        pick: Pick,
    },
    /// List the versions of the intent and diff sets, with approved and accepted
    #[command(
        after_help = "A diff set is accepted by approving its diff_set_weakened review items (cr review, cr answer);
its baseline, the version weakening is measured from, is the newest accepted version, else v1.

Examples:
  goodflows cr scenarios list                 # both sets, newest version first
  goodflows cr scenarios list --source diff   # only the diff set
  goodflows cr scenarios list --json          # the server's lists as JSON"
    )]
    List {
        #[command(flatten)]
        pick: Pick,
        /// Only this set
        #[arg(long, value_enum)]
        source: Option<SetSource>,
    },
    /// Approve the intent set: the current version, a set file's digest or a given digest; diff sets are accepted through cr answer
    #[command(after_help = "Examples:
  goodflows cr scenarios approve              # the current intent version
  goodflows cr scenarios approve qa-scenarios.md   # the digest of this file
  goodflows cr scenarios diff                 # review the changes first")]
    Approve {
        #[command(flatten)]
        pick: Pick,
        /// Approve the SHA-256 digest of this set file's bytes (default: the current intent version's digest)
        #[arg(conflicts_with = "digest")]
        file: Option<PathBuf>,
        /// Approve this digest, 64 lowercase hex characters (conflicts with FILE)
        #[arg(long, value_name = "SHA256")]
        digest: Option<String>,
    },
    /// Reject the intent set with a note: the current version, a set file's digest or a given digest; diff sets are answered through cr answer
    #[command(after_help = "Examples:
  goodflows cr scenarios reject --note S2-tests-the-wrong-path   # the current intent version
  goodflows cr scenarios reject qa-scenarios.md --note Missing-export   # the digest of this file
  goodflows cr scenarios approve              # approve a later version")]
    Reject {
        #[command(flatten)]
        pick: Pick,
        /// Reject the SHA-256 digest of this set file's bytes (default: the current intent version's digest)
        #[arg(conflicts_with = "digest")]
        file: Option<PathBuf>,
        /// Reject this digest, 64 lowercase hex characters (conflicts with FILE)
        #[arg(long, value_name = "SHA256")]
        digest: Option<String>,
        /// Why the set is wrong (required)
        #[arg(long, required = true)]
        note: String,
    },
    /// Show what changed between set versions by scenario ID, weakening changes first
    #[command(after_help = "Examples:
  goodflows cr scenarios diff                 # the current intent version against the approved one
  goodflows cr scenarios diff --source diff   # the current diff version against its baseline
  goodflows cr scenarios diff --from 1 --to 3 # two given versions")]
    Diff {
        #[command(flatten)]
        pick: Pick,
        /// Which set to compare: intent, the set approved before code, or diff, the set written from the change
        #[arg(long, value_enum, default_value_t = SetSource::Intent)]
        source: SetSource,
        /// Compare from this version (default: the newest approved intent version, or the diff baseline: the newest accepted diff version, else v1)
        #[arg(long, value_name = "VERSION")]
        from: Option<u64>,
        /// Compare to this version (default: the current one)
        #[arg(long, value_name = "VERSION")]
        to: Option<u64>,
    },
}

#[derive(Subcommand, Debug)]
pub enum RunsCommand {
    /// Upload a .verify/<app>/<run-id>/ run folder as a verify-run/1 document with its evidence
    #[command(
        mut_arg("number", |arg| arg.help("The change request whose verdict to show after the upload (default: the one draft or open change request of the current branch, if any)")),
        after_help = "The folder needs scenarios.md, the set the run graded, and run.md, whose YAML front matter has
format: \"verify-run-notes/1\", commit, branch, dirty, started_at, finished_at and results (id,
result, failed_step, reason, evidence paths), and may have set (path, sha256), changed_files,
features, drift, run_id, merge_result, links and workflow_run_id. The set digest is the SHA-256 of
scenarios.md.

Examples:
  goodflows cr runs upload .verify/web/20261119T143000Z-a1b2c3d   # upload the run and its evidence
  goodflows cr runs upload .verify/web/20261119T143000Z-a1b2c3d -n 42   # show change request 42's verdict
  goodflows cr runs upload .verify/web/20261119T143000Z-a1b2c3d --json   # print the result as JSON"
    )]
    Upload {
        /// The run folder, such as .verify/web/20261119T143000Z-a1b2c3d
        folder: PathBuf,
        #[command(flatten)]
        pick: Pick,
    },
}

#[derive(Subcommand, Debug)]
pub enum TasksCommand {
    /// Mark a task done
    #[command(after_help = "Examples:
  goodflows cr tasks done 5b1f                # mark task 5b1f done
  goodflows cr tasks done 5b1f -n 42          # mark task 5b1f of change request 42 done
  goodflows cr tasks done 5b1f --json         # print the task as JSON
  goodflows cr tasks                          # list what is left")]
    Done {
        /// The task id cr tasks lists
        task_id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum LeaseCommand {
    /// Take the merge slot for HEAD of an open, mergeable change request and print the lease id
    #[command(after_help = "Examples:
  goodflows cr lease acquire                  # merge onto origin/<target>
  goodflows cr lease acquire --base origin/main   # merge onto this commit
  goodflows cr lease acquire -n 42 --json     # change request 42; print the lease as JSON")]
    Acquire {
        #[command(flatten)]
        pick: Pick,
        /// The target branch commit to merge onto (default: origin/<target>, else <target>)
        #[arg(long, value_name = "REV")]
        base: Option<String>,
        /// Skip git fetch origin <target> when resolving the default base
        #[arg(long, conflicts_with = "base")]
        no_fetch: bool,
    },
    /// Extend a lease by the policy's lease TTL; run it before the lease expires
    #[command(after_help = "Examples:
  goodflows cr lease heartbeat 7c0e           # extend lease 7c0e
  goodflows cr lease heartbeat 7c0e --json    # print the lease as JSON
  goodflows cr lease release 7c0e             # free the slot when done")]
    Heartbeat {
        /// The lease id cr lease acquire printed
        lease_id: String,
    },
    /// Release a lease and free the slot
    #[command(after_help = "Examples:
  goodflows cr lease release 7c0e             # free the slot
  goodflows cr lease release 7c0e --json      # print the result as JSON
  goodflows cr lease acquire                  # take it again")]
    Release {
        /// The lease id cr lease acquire printed
        lease_id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum PolicyCommand {
    /// Show the project's review policy (TOML), its version and active settings
    #[command(after_help = "Examples:
  goodflows policy get                        # the current policy as TOML, ready to edit
  goodflows policy get --version 2            # a stored version
  goodflows policy get --effective            # every setting with defaults filled in")]
    Get {
        /// A stored version (default: the current one)
        #[arg(long, value_name = "N")]
        version: Option<u64>,
        /// Show every setting with defaults filled in, not the document as written
        #[arg(long)]
        effective: bool,
    },
    /// Write a new policy version from a review-policy/1 TOML file (owners and admins)
    #[command(after_help = "Examples:
  goodflows policy set policy.toml            # replace the current version
  goodflows policy set policy.toml --previous-version 3   # fail if another write landed after 3
  goodflows policy set policy.toml --note Require-ci   # record why")]
    Set {
        /// The review-policy/1 TOML file (format defaults to review-policy/1); start from policy get
        file: PathBuf,
        /// The version this one replaces; a write fails when another landed first (default: the current version)
        #[arg(long, value_name = "N")]
        previous_version: Option<u64>,
        /// Why this version was written, stored with it
        #[arg(long)]
        note: Option<String>,
    },
    /// List policy versions, newest first
    #[command(after_help = "Examples:
  goodflows policy history                    # the newest 50 versions
  goodflows policy history --limit 5          # the newest 5
  goodflows policy history --json             # as JSON, with next_cursor")]
    History {
        /// Versions per page, 1-200 (server default 50)
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u16).range(1..=200))]
        limit: Option<u16>,
        /// The next page, as printed after the previous one
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Show merged change requests and defects by lane, policy version, rule and path glob
    #[command(after_help = "Examples:
  goodflows policy calibration                # the last 90 days
  goodflows policy calibration --from 2026-10-01T00:00:00Z   # since October
  goodflows policy calibration --json         # the report as JSON")]
    Calibration {
        /// Start of the window (ISO 8601; default 90 days before --to)
        #[arg(long)]
        from: Option<String>,
        /// End of the window (ISO 8601; default now)
        #[arg(long)]
        to: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum FeaturesCommand {
    /// Upload the verify skill's feature files (id, title, paths globs) as a new feature index version
    #[command(
        after_help = "Each feature file's front matter may set id (default: the file name), title (default: its
# heading) and paths, the globs of the code it covers. Files without paths are skipped.

Examples:
  goodflows features sync                     # the one .agents/skills/verify-*/features directory
  goodflows features sync --dir .claude/skills/verify-web/features   # another directory
  goodflows features sync --json              # print the version and features as JSON"
    )]
    Sync {
        /// The features directory (default: the one verify-*/features directory in .agents/skills, .claude/skills or .cursor/skills)
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Check the feature files as features sync reads them, without a server, and list every problem
    #[command(
        after_help = "Checks each feature file's front matter: valid YAML, id and title strings, and paths a list of
globs. Also checks that no two files with paths share an id and that some file has paths. A file
without paths is a warning, since features sync skips it. Exits 1 when anything sync would refuse
is found. Needs no API key or goodflows.toml.

Examples:
  goodflows features validate                 # the one .agents/skills/verify-*/features directory
  goodflows features validate --dir .claude/skills/verify-web/features   # another directory
  goodflows features validate --json          # every file's errors and warnings as JSON"
    )]
    Validate {
        /// The features directory (default: the one verify-*/features directory in a skills directory)
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

#[derive(Args, Debug)]
pub struct EventsArgs {
    /// Return events after this cursor, an event id such as the next cursor printed (default: from the oldest kept)
    #[arg(long, value_name = "CURSOR")]
    after: Option<u64>,
    /// Seconds to wait for an event when none is ready, 0-30 (default: return at once; 30 with --follow)
    #[arg(long, value_name = "SECS", value_parser = clap::value_parser!(u8).range(0..=30))]
    wait: Option<u8>,
    /// Events per page, 1-200
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u16).range(1..=200))]
    limit: Option<u16>,
    /// Keep reading with 30-second long-polls until interrupted
    #[arg(long)]
    follow: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
#[value(rename_all = "snake_case")]
pub enum VerdictState {
    Pending,
    Blocked,
    NeedsReview,
    Mergeable,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
#[value(rename_all = "snake_case")]
pub enum AnswerKind {
    Approve,
    Reject,
    RequestChange,
    Choose,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum TaskState {
    Open,
    Stale,
    Done,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum SetSource {
    Intent,
    Diff,
}

fn name<T: ValueEnum>(value: T) -> String {
    value
        .to_possible_value()
        .expect("no skipped values")
        .get_name()
        .to_owned()
}

/// The server's change id rule.
fn change_id(value: &str) -> Result<String, String> {
    let ok = (1..=64).contains(&value.len())
        && value.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(value.to_owned())
    } else {
        Err("must be 1-64 characters of a-z, 0-9 and -, starting with a letter or digit".into())
    }
}

fn valid_text(value: &str) -> bool {
    (1..=256).contains(&value.len()) && !value.chars().any(char::is_control)
}

fn lowercase_hex(value: &str, lengths: &[usize]) -> bool {
    lengths.contains(&value.len())
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

struct Ctx<'a> {
    api: Api,
    git: Git,
    output: &'a Output,
}

impl<'a> Ctx<'a> {
    fn new(
        reporting: Option<&Reporting>,
        command: &str,
        output: &'a Output,
        cancel: watch::Receiver<Option<i32>>,
    ) -> Result<Self> {
        let api = Api::new(reporting, command, cancel)?;
        let git = Git::new(api.token_env());
        Ok(Self { api, git, output })
    }

    async fn emit(&self, event: Value) -> Result<()> {
        self.output.emit(event).await
    }

    /// The draft and open change requests whose source branch is `branch`.
    async fn live_on_branch(&self, branch: &str) -> Result<Vec<Value>> {
        self.on_branch(branch, &["draft", "open"]).await
    }

    /// The change requests in `states` whose source branch is `branch`.
    async fn on_branch(&self, branch: &str, states: &[&str]) -> Result<Vec<Value>> {
        let mut found = Vec::new();
        for &state in states {
            let mut cursor: Option<String> = None;
            loop {
                let mut query = vec![
                    ("state", state.to_owned()),
                    ("source_branch", branch.to_owned()),
                    ("limit", "200".to_owned()),
                ];
                if let Some(cursor) = &cursor {
                    query.push(("cursor", cursor.clone()));
                }
                let page = self.api.get(&["change-requests"], &query).await?;
                let items = page["data"].as_array().ok_or_else(|| {
                    anyhow!("the server's response is not a list of change requests")
                })?;
                found.extend(items.iter().cloned());
                match page["next_cursor"].as_str() {
                    Some(next) => cursor = Some(next.to_owned()),
                    None => break,
                }
            }
        }
        found.sort_by_key(|cr| cr["number"].as_u64());
        Ok(found)
    }

    /// The one draft or open change request of the current branch, if any.
    async fn on_current_branch(&self) -> Result<Option<Value>> {
        let branch = self.current_branch().await?;
        let mut found = self.live_on_branch(&branch).await?;
        match found.len() {
            0 => Ok(None),
            1 => Ok(found.pop()),
            _ => bail!(
                "branch {} has several draft or open change requests ({}); pass --number",
                crate::output::visible(&branch),
                numbers(&found)
            ),
        }
    }

    async fn current_branch(&self) -> Result<String> {
        self.git.branch().await.ok_or_else(|| {
            anyhow!("HEAD is detached; check out the change request's branch or pass --number")
        })
    }

    /// `pick`'s number, or the change request of the current branch.
    async fn number(&self, pick: Pick) -> Result<u64> {
        if let Some(number) = pick.number {
            return Ok(number);
        }
        let branch = self.current_branch().await?;
        match self.on_current_branch().await? {
            Some(cr) => number_of(&cr),
            None => bail!(
                "branch {} has no draft or open change request; open one with goodflows cr open, or pass --number",
                crate::output::visible(&branch)
            ),
        }
    }

    /// `pick`'s number, or the one closed change request of the current branch.
    async fn closed_number(&self, pick: Pick) -> Result<u64> {
        if let Some(number) = pick.number {
            return Ok(number);
        }
        let branch = self.current_branch().await?;
        let found = self.on_branch(&branch, &["closed"]).await?;
        match found.as_slice() {
            [cr] => number_of(cr),
            [] => bail!(
                "branch {} has no closed change request; pass --number",
                crate::output::visible(&branch)
            ),
            _ => bail!(
                "branch {} has several closed change requests ({}); pass --number",
                crate::output::visible(&branch),
                numbers(&found)
            ),
        }
    }

    async fn show(&self, number: u64) -> Result<Value> {
        let body = self
            .api
            .get(&["change-requests", &number.to_string()], &[])
            .await?;
        data(body, "show")
    }

    async fn verdict(&self, number: u64) -> Result<Value> {
        let body = self
            .api
            .get(&["change-requests", &number.to_string(), "verdict"], &[])
            .await?;
        data(body, "verdict")
    }

    /// Report `git-facts/1` for HEAD against the change request's target
    /// branch, after fetching it unless `no_fetch`. Returns the summary,
    /// `reported` when new and `unchanged` when the server had it, and the
    /// change request the server returned, at the new head.
    async fn report_facts(&self, cr: &Value, no_fetch: bool) -> Result<(Value, Value)> {
        let number = number_of(cr)?;
        let target = cr["target_branch"]
            .as_str()
            .ok_or_else(|| anyhow!("the change request has no target branch"))?;
        if !no_fetch {
            self.git.fetch_target(target).await?;
        }
        let facts = self.git.facts(target).await?;
        let head = facts["head"].as_str().unwrap_or_default();
        let (status, body) = self
            .api
            .send(
                Method::POST,
                &["change-requests", &number.to_string(), "git-facts"],
                &[],
                Body::Json(facts.clone()),
            )
            .await
            .map_err(|error| {
                explain_conflict(error, |errors| {
                    (errors["detail"] == GIT_FACTS_CONFLICT).then(|| {
                        format!(
                            "{} failed: Git facts for HEAD {} against {} were already reported with a different merge base; make a new commit (or retarget) and report again",
                            self.api.command(),
                            &head[..head.len().min(12)],
                            crate::output::visible(target)
                        )
                    })
                })
            })?;
        let updated = data(body, self.api.command())?;
        let summary = json!({
            "head": facts["head"],
            "target": facts["target"],
            "commits": facts["commits"].as_array().map_or(0, Vec::len),
            "files": facts["diff"]["files"].as_array().map_or(0, Vec::len),
            "status": if status == 201 { "reported" } else { "unchanged" },
        });
        Ok((summary, updated))
    }

    /// Sets the change request's dependencies to the `depends_on` of its
    /// linked change's `change.md` at `head`, leaving out changes archived
    /// at `head`, when that differs from the stored list. Sends nothing when
    /// no change is linked or `change.md` is not in `head`. Returns what was
    /// read, with `updated` or `unchanged`, and the change request after.
    async fn sync_dependencies(&self, cr: Value, head: &str) -> Result<(Value, Value)> {
        let Some(change) = cr["change_id"].as_str() else {
            return Ok((Value::Null, cr));
        };
        let file = format!("goodflows/changes/{change}/change.md");
        let Some(text) = self
            .git
            .try_output(&["show", &format!("{head}:{file}")])
            .await
        else {
            return Ok((Value::Null, cr));
        };
        let archive: Vec<String> = self
            .git
            .try_output(&[
                "ls-tree",
                "--name-only",
                &format!("{head}:goodflows/changes/archive"),
            ])
            .await
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        let ids = dependencies_at(&text, &archive)
            .ok_or_else(|| anyhow!("{file} at {head} has no readable front matter; check it with goodflows change validate {change}"))?;
        let stored: Vec<&str> = cr["dependencies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let same = stored.len() == ids.len() && ids.iter().all(|id| stored.contains(&id.as_str()));
        let summary = |status: &str| json!({"file": file, "change_ids": ids, "status": status});
        if same {
            return Ok((summary("unchanged"), cr));
        }
        let number = number_of(&cr)?.to_string();
        let (_, response) = self
            .api
            .send(
                Method::PUT,
                &["change-requests", number.as_str(), "dependencies"],
                &[],
                Body::Json(json!({"change_ids": ids})),
            )
            .await?;
        let updated = data(response, self.api.command())?;
        Ok((summary("updated"), updated))
    }

    /// Fails unless the claims describe HEAD, which the server requires.
    async fn claims_for_head(&self, claims: &markdown::Claims, file: &Path) -> Result<String> {
        let head = self.git.head().await?;
        if claims.head != head {
            bail!(
                "{} describes {} but HEAD is {head}; update the claims for HEAD first",
                file.display(),
                crate::output::visible(&claims.head)
            );
        }
        Ok(head)
    }

    /// Check the claims describe HEAD, fill in `cr` when it is null, and
    /// upload the file as it now is. Returns the summary and the change
    /// request the server returned.
    async fn upload_claims(&self, number: u64, file: &Path, text: &str) -> Result<(Value, Value)> {
        let shown = file.display().to_string();
        let claims = markdown::claims(text, &shown)?;
        let head = self.claims_for_head(&claims, file).await?;
        let wrote_cr = match claims.cr {
            Some(n) if n == number => false,
            Some(n) => bail!("{shown} names change request {n}, not {number}"),
            None => true,
        };
        let text = if wrote_cr {
            let updated = markdown::set_cr(text, number, &shown)?;
            std::fs::write(file, &updated).with_context(|| format!("could not write {shown}"))?;
            updated
        } else {
            text.to_owned()
        };
        let (status, body) = self
            .api
            .send(
                Method::POST,
                &["change-requests", &number.to_string(), "claims"],
                &[],
                Body::Markdown(text.into_bytes()),
            )
            .await
            .map_err(|error| {
                explain_conflict(error, |errors| {
                    (errors["detail"] == CLAIMS_CONFLICT).then(|| {
                        format!(
                            "{} failed: different claims are already stored for HEAD {}; claims are fixed per head, so commit the change, update the file's head to the new HEAD, and upload again",
                            self.api.command(),
                            &head[..head.len().min(12)]
                        )
                    })
                })
            })?;
        let updated = data(body, self.api.command())?;
        let summary = json!({
            "file": shown,
            "head": head,
            "status": if status == 201 { "uploaded" } else { "unchanged" },
            "wrote_cr": wrote_cr,
        });
        Ok((summary, updated))
    }

    async fn default_claims(&self) -> Result<PathBuf> {
        let top = self.git.toplevel().await?;
        Ok(self.git.claims_path().await?.absolute(&top))
    }
}

fn number_of(cr: &Value) -> Result<u64> {
    cr["number"]
        .as_u64()
        .ok_or_else(|| anyhow!("the server's change request has no number"))
}

fn numbers(crs: &[Value]) -> String {
    crs.iter()
        .map(|cr| format!("#{}", cr["number"]))
        .collect::<Vec<_>>()
        .join(", ")
}

fn read_text(file: &Path) -> Result<String> {
    let bytes =
        std::fs::read(file).with_context(|| format!("could not read {}", file.display()))?;
    String::from_utf8(bytes).map_err(|_| anyhow!("{} is not UTF-8", file.display()))
}

pub async fn cr(
    reporting: Option<&Reporting>,
    command: &CrCommand,
    output: &Output,
    cancel: watch::Receiver<Option<i32>>,
) -> Result<u8> {
    if let CrCommand::Path = command {
        // Needs no server.
        let git = Git::new(reporting.map_or("GOODFLOWS_API_KEY", |r| r.token_env.as_str()));
        let top = git.toplevel().await?;
        let path = git.claims_path().await?;
        let scenarios = path.scenarios();
        output
            .emit(json!({
                "type": "change_request_path",
                "path": path.relative.to_string_lossy(),
                "absolute_path": path.absolute(&top).to_string_lossy(),
                "branch": path.branch,
                "exists": path.absolute(&top).is_file(),
                "scenarios_path": scenarios.to_string_lossy(),
                "scenarios_absolute_path": top.join(&scenarios).to_string_lossy(),
                "scenarios_exists": top.join(&scenarios).is_file(),
            }))
            .await?;
        return Ok(0);
    }
    if let CrCommand::Tasks {
        state: Some(_),
        command: Some(_),
        ..
    } = command
    {
        return Err(crate::usage("--state cannot be used with cr tasks done"));
    }
    if let CrCommand::Validate { paths } = command {
        // Needs no server.
        let git = Git::new(reporting.map_or("GOODFLOWS_API_KEY", |r| r.token_env.as_str()));
        return validate(paths, &git, output).await;
    }
    let label = format!("cr {}", command_name(command));
    let ctx = Ctx::new(reporting, &label, output, cancel)?;
    match command {
        CrCommand::Path | CrCommand::Validate { .. } => unreachable!(),
        CrCommand::Open {
            claims,
            change_id,
            title,
            target,
            no_fetch,
        } => {
            open(
                &ctx,
                claims.as_deref(),
                change_id.as_deref(),
                title.as_deref(),
                target.as_deref(),
                *no_fetch,
            )
            .await
        }
        CrCommand::Claims {
            file,
            pick,
            no_fetch,
        } => update_claims(&ctx, file.as_deref(), *pick, *no_fetch).await,
        CrCommand::Push {
            pick,
            git_args,
            no_fetch,
        } => push(&ctx, *pick, git_args, *no_fetch).await,
        CrCommand::Status { pick } => {
            let number = ctx.number(*pick).await?;
            let cr = ctx.show(number).await?;
            ctx.emit(json!({"type": "change_request", "action": "status", "change_request": cr}))
                .await?;
            Ok(0)
        }
        CrCommand::Verdict { pick } => {
            let number = ctx.number(*pick).await?;
            let verdict = ctx.verdict(number).await?;
            let local_head = ctx.git.head().await.ok();
            ctx.emit(json!({"type": "change_request_verdict", "number": number, "verdict": verdict, "local_head": local_head}))
                .await?;
            Ok(0)
        }
        CrCommand::Wait {
            pick,
            until,
            timeout,
            interval,
        } => wait(&ctx, *pick, until, *timeout, *interval).await,
        CrCommand::Ready { pick }
        | CrCommand::Draft { pick }
        | CrCommand::Close { pick }
        | CrCommand::Reopen { pick } => {
            let action = command_name(command);
            let number = if matches!(command, CrCommand::Reopen { .. }) {
                ctx.closed_number(*pick).await?
            } else {
                ctx.number(*pick).await?
            };
            let body = ctx
                .api
                .post(
                    &["change-requests", &number.to_string(), action],
                    Body::None,
                )
                .await?;
            let cr = data(body, action)?;
            ctx.emit(json!({"type": "change_request", "action": action, "change_request": cr}))
                .await?;
            Ok(0)
        }
        CrCommand::Scenarios { command } => scenarios(&ctx, command).await,
        CrCommand::Runs {
            command: RunsCommand::Upload { folder, pick },
        } => upload_run(&ctx, folder, *pick).await,
        CrCommand::Review { pick } => {
            let number = ctx.number(*pick).await?;
            let body = ctx
                .api
                .get(&["change-requests", &number.to_string(), "review"], &[])
                .await?;
            let packet = data(body, "review")?;
            ctx.emit(json!({"type": "review_packet", "number": number, "packet": packet}))
                .await?;
            Ok(0)
        }
        CrCommand::Answer {
            pick,
            item_id,
            answer,
            option,
            note,
            head,
        } => {
            let number = ctx.number(*pick).await?;
            match (answer, option) {
                (AnswerKind::Choose, None) => bail!("choose needs --option"),
                (AnswerKind::Choose, Some(_)) | (_, None) => {}
                (_, Some(_)) => bail!("--option is only for choose"),
            }
            if matches!(answer, AnswerKind::Reject | AnswerKind::RequestChange) && note.is_none() {
                bail!("{} needs --note", name(*answer));
            }
            let head = match head {
                Some(head) => json!(head),
                None => ctx.show(number).await?["head"].clone(),
            };
            let mut body = json!({"head": head, "answer": name(*answer)});
            if let Some(option) = option {
                body["option"] = json!(option);
            }
            if let Some(note) = note {
                body["note"] = json!(note);
            }
            let response = ctx
                .api
                .post(
                    &[
                        "change-requests",
                        &number.to_string(),
                        "review",
                        "items",
                        item_id,
                        "answers",
                    ],
                    Body::Json(body),
                )
                .await
                .map_err(|error| {
                    explain_conflict(error, |errors| {
                        (errors.get("head").is_some() && errors.get("error").is_none()).then(|| {
                            format!(
                                "{} failed: change request #{number} moved to head {} since the answer's head; read goodflows cr review again and answer at the new head",
                                ctx.api.command(),
                                quoted(&errors["head"])
                            )
                        })
                    })
                })?;
            let answer = data(response, "answer")?;
            ctx.emit(json!({"type": "review_answer", "number": number, "item_id": item_id, "answer": answer}))
                .await?;
            Ok(0)
        }
        CrCommand::Tasks {
            pick,
            state,
            command,
        } => {
            let number = ctx.number(*pick).await?.to_string();
            if let Some(TasksCommand::Done { task_id }) = command {
                let body = ctx
                    .api
                    .post(
                        &["change-requests", &number, "tasks", task_id, "done"],
                        Body::None,
                    )
                    .await?;
                let task = data(body, "tasks done")?;
                ctx.emit(json!({"type": "change_request_task", "task": task}))
                    .await?;
            } else {
                let query: Vec<_> = state.map(|s| ("state", name(s))).into_iter().collect();
                let body = ctx
                    .api
                    .get(&["change-requests", &number, "tasks"], &query)
                    .await?;
                let tasks = data(body, "tasks")?;
                ctx.emit(json!({"type": "change_request_tasks", "number": number.parse::<u64>()?, "tasks": tasks}))
                    .await?;
            }
            Ok(0)
        }
        CrCommand::Merged {
            pick,
            commit,
            target,
            lease,
        } => {
            let number = ctx.number(*pick).await?;
            let commit = if lowercase_hex(commit, &[40, 64]) {
                commit.clone()
            } else {
                ctx.git.resolve(commit).await?
            };
            let target = match target {
                Some(target) => target.clone(),
                None => ctx.show(number).await?["target_branch"]
                    .as_str()
                    .ok_or_else(|| anyhow!("the change request has no target branch"))?
                    .to_owned(),
            };
            let mut body = json!({"merged_commit": commit, "target_branch": target});
            if let Some(lease) = lease {
                body["lease_id"] = json!(lease);
            }
            let response = ctx
                .api
                .post(
                    &["change-requests", &number.to_string(), "merged"],
                    Body::Json(body),
                )
                .await?;
            let cr = data(response, "merged")?;
            ctx.emit(json!({"type": "change_request", "action": "merged", "change_request": cr}))
                .await?;
            Ok(0)
        }
        CrCommand::Retarget { pick, branch } => {
            if !valid_text(branch) {
                bail!("the target branch must be 1-256 bytes without control characters");
            }
            let number = ctx.number(*pick).await?;
            let response = ctx
                .api
                .post(
                    &["change-requests", &number.to_string(), "retarget"],
                    Body::Json(json!({"target_branch": branch})),
                )
                .await?;
            let cr = data(response, "retarget")?;
            ctx.emit(json!({"type": "change_request", "action": "retarget", "change_request": cr}))
                .await?;
            Ok(0)
        }
        CrCommand::Parent {
            pick,
            parent,
            clear,
        } => {
            let number = ctx.number(*pick).await?.to_string();
            let segments = ["change-requests", number.as_str(), "parent"];
            let (_, response) = if *clear {
                ctx.api
                    .send(Method::DELETE, &segments, &[], Body::None)
                    .await?
            } else {
                ctx.api
                    .send(
                        Method::PUT,
                        &segments,
                        &[],
                        Body::Json(json!({"number": parent})),
                    )
                    .await?
            };
            let cr = data(response, "parent")?;
            ctx.emit(json!({"type": "change_request", "action": "parent", "change_request": cr}))
                .await?;
            Ok(0)
        }
        CrCommand::Lease { command } => lease(&ctx, command).await,
        CrCommand::Defect {
            number,
            commit,
            ticket,
            severity,
        } => {
            let mut body = match (number, commit) {
                (Some(number), _) => json!({"change_request": number}),
                (None, Some(commit)) => json!({"commit": ctx.git.resolve(commit).await?}),
                (None, None) => unreachable!("clap requires one"),
            };
            if let Some(ticket) = ticket {
                body["ticket"] = json!(ticket);
            }
            if let Some(severity) = severity {
                body["severity"] = json!(name(*severity));
            }
            let response = ctx.api.post(&["defects"], Body::Json(body)).await?;
            let defect = data(response, "defect")?;
            ctx.emit(json!({"type": "defect", "defect": defect}))
                .await?;
            Ok(0)
        }
    }
}

/// `cr validate`: each file checked as the command that uploads it reads
/// it, with every problem that command would refuse and warnings for what
/// it would trip on later.
async fn validate(paths: &[PathBuf], git: &Git, output: &Output) -> Result<u8> {
    let paths = if paths.is_empty() {
        discover(git).await?
    } else {
        paths.to_vec()
    };
    let head = git.head().await.ok();
    let files: Vec<Value> = paths
        .iter()
        .map(|path| check(path, head.as_deref()))
        .collect();
    let ok = files.iter().all(|file| file["ok"] == true);
    output
        .emit(json!({"type": "cr_validation", "ok": ok, "files": files}))
        .await?;
    Ok(if ok { 0 } else { 1 })
}

/// The change request files of the checkout: the branch's claims file and
/// diff set, each goodflows change's `qa-scenarios.md`, and every
/// `.verify/<app>/<run-id>/` run folder, relative to the working directory
/// when they are under it.
async fn discover(git: &Git) -> Result<Vec<PathBuf>> {
    let top = git.toplevel().await?;
    let mut found = Vec::new();
    let path = git.claims_path().await?;
    for file in [path.absolute(&top), top.join(path.scenarios())] {
        if file.is_file() {
            found.push(file);
        }
    }
    let children = |dir: PathBuf| -> Vec<PathBuf> {
        let mut paths: Vec<_> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        paths.sort();
        paths
    };
    for change in children(top.join("goodflows/changes")) {
        let set = change.join("qa-scenarios.md");
        if change.file_name().is_some_and(|n| n != "archive") && set.is_file() {
            found.push(set);
        }
    }
    for app in children(top.join(".verify")) {
        found.extend(
            children(app)
                .into_iter()
                .filter(|run| run.join("run.md").is_file()),
        );
    }
    if found.is_empty() {
        bail!("found no change request files (the claims file at goodflows cr path, goodflows/changes/*/qa-scenarios.md or .verify/<app>/<run-id>/ run folders); pass the files to check");
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    Ok(found
        .into_iter()
        .map(|path| {
            path.strip_prefix(&cwd)
                .map_or(path.clone(), Path::to_path_buf)
        })
        .collect())
}

/// One file's result: its kind, the problems that block its upload, and
/// warnings.
fn check(path: &Path, head: Option<&str>) -> Value {
    let shown = path.display().to_string();
    let mut warnings = Vec::new();
    let (kind, result) = check_file(path, &shown, head, &mut warnings);
    let errors = result.err().map(markdown::problems).unwrap_or_default();
    json!({
        "file": shown,
        "kind": kind,
        "ok": errors.is_empty(),
        "errors": errors,
        "warnings": warnings,
    })
}

fn check_file(
    path: &Path,
    shown: &str,
    head: Option<&str>,
    warnings: &mut Vec<String>,
) -> (&'static str, Result<()>) {
    if path.is_dir() {
        if !path.join("run.md").is_file() {
            return (
                "unknown",
                Err(anyhow!("{shown} is a directory without run.md; pass a .verify/<app>/<run-id>/ run folder or a file")),
            );
        }
        return ("verify_run", verify_run::read(path).map(drop));
    }
    let text = match read_text(path) {
        Ok(text) => text,
        Err(error) => return ("unknown", Err(error)),
    };
    let format = markdown::split(&text)
        .and_then(|front| markdown::parse(front.yaml, shown).ok())
        .and_then(|fields| fields["format"].as_str().map(str::to_owned));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // The format names the kind; a file whose format is missing or wrong is
    // checked as its usual name suggests, so its problems are reported.
    let kind = match format.as_deref() {
        Some(markdown::CLAIMS_FORMAT) => "claims",
        Some(markdown::SET_FORMAT) => "scenario_set",
        Some(verify_run::NOTES_FORMAT) => "verify_run",
        _ if name == "cr.md" => "claims",
        _ if name.ends_with("scenarios.md") => "scenario_set",
        _ if name == "run.md" => "verify_run",
        _ => "unknown",
    };
    let result = match kind {
        "claims" => check_claims(&text, shown, head, warnings),
        "scenario_set" => check_set(&text, shown, warnings),
        "verify_run" => verify_run::read(path.parent().unwrap_or(Path::new("."))).map(drop),
        _ if in_features_dir(path) => Err(anyhow!(
            "{shown} is a feature file; check it with goodflows features validate"
        )),
        _ => Err(anyhow!(
            "{shown}: the front matter's format must be {}, {} or {}",
            markdown::CLAIMS_FORMAT,
            markdown::SET_FORMAT,
            verify_run::NOTES_FORMAT
        )),
    };
    (kind, result)
}

fn check_claims(
    text: &str,
    shown: &str,
    head: Option<&str>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let claims = markdown::claims(text, shown)?;
    if claims.cr.is_none() && markdown::set_cr(text, 1, shown).is_err() {
        bail!("{shown}: the front matter needs a top-level cr: null line for cr open to fill in");
    }
    if let Some(head) = head.filter(|h| *h != claims.head) {
        warnings.push(format!(
            "{shown}: head is {} but HEAD is {head}; cr claims uploads claims only for HEAD",
            crate::output::visible(&claims.head)
        ));
    }
    warnings.extend(markdown::claims_server_warnings(text, shown));
    if claims.cr.is_none() && claims.title.is_none() {
        warnings.push(format!(
            "{shown}: the body has no # heading, so cr open needs --title"
        ));
    }
    Ok(())
}

fn check_set(text: &str, shown: &str, warnings: &mut Vec<String>) -> Result<()> {
    let set = markdown::scenario_set(text, shown)?;
    markdown::sections(text, &set.ids, shown)?;
    if set.dirty {
        warnings.push(format!("{shown}: revision ends +dirty, so cr scenarios upload refuses it until the changes are committed and the set is written again"));
    }
    Ok(())
}

/// A file in a verify skill's `verify-*/features` directory.
fn in_features_dir(path: &Path) -> bool {
    let dir = path.parent();
    dir.and_then(Path::file_name)
        .is_some_and(|n| n == "features")
        && dir
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_some_and(|n| n.to_string_lossy().starts_with("verify-"))
}

fn command_name(command: &CrCommand) -> &'static str {
    match command {
        CrCommand::Open { .. } => "open",
        CrCommand::Claims { .. } => "claims",
        CrCommand::Push { .. } => "push",
        CrCommand::Status { .. } => "status",
        CrCommand::Path => "path",
        CrCommand::Validate { .. } => "validate",
        CrCommand::Verdict { .. } => "verdict",
        CrCommand::Wait { .. } => "wait",
        CrCommand::Ready { .. } => "ready",
        CrCommand::Draft { .. } => "draft",
        CrCommand::Close { .. } => "close",
        CrCommand::Reopen { .. } => "reopen",
        CrCommand::Scenarios { command } => match command {
            ScenariosCommand::Upload { .. } => "scenarios upload",
            ScenariosCommand::List { .. } => "scenarios list",
            ScenariosCommand::Approve { .. } => "scenarios approve",
            ScenariosCommand::Reject { .. } => "scenarios reject",
            ScenariosCommand::Diff { .. } => "scenarios diff",
        },
        CrCommand::Runs { .. } => "runs upload",
        CrCommand::Review { .. } => "review",
        CrCommand::Answer { .. } => "answer",
        CrCommand::Tasks { .. } => "tasks",
        CrCommand::Merged { .. } => "merged",
        CrCommand::Retarget { .. } => "retarget",
        CrCommand::Parent { .. } => "parent",
        CrCommand::Lease { command } => match command {
            LeaseCommand::Acquire { .. } => "lease acquire",
            LeaseCommand::Heartbeat { .. } => "lease heartbeat",
            LeaseCommand::Release { .. } => "lease release",
        },
        CrCommand::Defect { .. } => "defect",
    }
}

async fn open(
    ctx: &Ctx<'_>,
    claims_file: Option<&Path>,
    change_id: Option<&str>,
    title: Option<&str>,
    target: Option<&str>,
    no_fetch: bool,
) -> Result<u8> {
    let branch = ctx.git.branch().await.ok_or_else(|| {
        anyhow!("HEAD is detached; cr open needs the change request's branch checked out")
    })?;
    if !valid_text(&branch) {
        bail!("the branch name must be 1-256 bytes without control characters");
    }
    let file = match claims_file {
        Some(file) => {
            if !file.is_file() {
                bail!("the claims file {} does not exist", file.display());
            }
            file.to_path_buf()
        }
        None => ctx.default_claims().await?,
    };
    let text = file.is_file().then(|| read_text(&file)).transpose()?;
    let claims = text
        .as_deref()
        .map(|text| markdown::claims(text, &file.display().to_string()))
        .transpose()?;
    if let Some(claims) = &claims {
        ctx.claims_for_head(claims, &file).await?;
    }
    let mut found = ctx.live_on_branch(&branch).await?;
    if found.len() > 1 {
        bail!(
            "branch {} has several draft or open change requests ({}); close the extra ones or use cr claims with the file's cr",
            crate::output::visible(&branch),
            numbers(&found)
        );
    }
    let (cr, created) = match found.pop() {
        Some(cr) => {
            if let Some(wanted) = change_id {
                let linked = cr["change_id"].as_str();
                if linked != Some(wanted) {
                    bail!(
                        "change request #{} of this branch links change {}, not {wanted}; the link cannot be changed",
                        cr["number"],
                        linked.map_or("none".to_owned(), |l| crate::output::visible(l).into_owned())
                    );
                }
            }
            (cr, false)
        }
        None => {
            if let Some(n) = claims.as_ref().and_then(|c| c.cr) {
                bail!(
                    "{} names change request {n}, which is not a draft or open change request of branch {}; reopen it, or set cr: null to open a new one",
                    file.display(),
                    crate::output::visible(&branch)
                );
            }
            let title = match (title, claims.as_ref().and_then(|c| c.title.as_deref())) {
                (Some(title), _) | (None, Some(title)) => title.to_owned(),
                (None, None) if claims.is_some() => bail!(
                    "{} has no # heading for the title; pass --title",
                    file.display()
                ),
                (None, None) => bail!(
                    "there is no claims file at {}; pass --title to open a change request without one",
                    file.display()
                ),
            };
            if !valid_text(&title) {
                bail!("the title must be 1-256 bytes without control characters");
            }
            let mut body = json!({"title": title, "source_branch": branch});
            if let Some(change) = change_id {
                body["change_id"] = json!(change);
            }
            if let Some(target) = target {
                body["target_branch"] = json!(target);
            }
            let response = ctx.api.post(&["change-requests"], Body::Json(body)).await?;
            (data(response, "open")?, true)
        }
    };
    let number = number_of(&cr)?;
    if let Some(n) = claims.as_ref().and_then(|c| c.cr) {
        if n != number {
            bail!(
                "{} names change request {n}, but branch {} has change request #{number}",
                file.display(),
                crate::output::visible(&branch)
            );
        }
    }
    let mut event = json!({
        "type": "change_request",
        "action": "open",
        "created": created,
        "change_request": cr,
    });
    let head = if let Some(text) = text {
        let (facts, _) = ctx.report_facts(&cr, no_fetch).await?;
        let (uploaded, latest) = ctx.upload_claims(number, &file, &text).await?;
        let head = facts["head"].as_str().unwrap_or_default().to_owned();
        event["git_facts"] = facts;
        event["claims"] = uploaded;
        event["change_request"] = latest;
        head
    } else {
        event["claims"] = json!({"file": file.display().to_string(), "status": "missing"});
        ctx.git.head().await?
    };
    let (dependencies, latest) = ctx
        .sync_dependencies(event["change_request"].take(), &head)
        .await?;
    if event["git_facts"].is_object() {
        event["verdict"] = latest["verdict"].clone();
    }
    event["dependencies"] = dependencies;
    event["change_request"] = latest;
    ctx.emit(event).await?;
    Ok(0)
}

async fn update_claims(
    ctx: &Ctx<'_>,
    file: Option<&Path>,
    pick: Pick,
    no_fetch: bool,
) -> Result<u8> {
    let file = match file {
        Some(file) => file.to_path_buf(),
        None => ctx.default_claims().await?,
    };
    if !file.is_file() {
        bail!(
            "the claims file {} does not exist; write it first (gf-change-request)",
            file.display()
        );
    }
    let text = read_text(&file)?;
    let claims = markdown::claims(&text, &file.display().to_string())?;
    ctx.claims_for_head(&claims, &file).await?;
    let number = match (claims.cr, pick.number) {
        (Some(n), Some(m)) if n != m => {
            bail!("{} names change request {n}, not {m}", file.display())
        }
        (Some(n), _) => n,
        (None, _) => ctx.number(pick).await?,
    };
    let cr = ctx.show(number).await?;
    let (facts, _) = ctx.report_facts(&cr, no_fetch).await?;
    let (uploaded, latest) = ctx.upload_claims(number, &file, &text).await?;
    ctx.emit(json!({
        "type": "change_request",
        "action": "claims",
        "change_request": latest,
        "git_facts": facts,
        "claims": uploaded,
        "verdict": latest["verdict"],
    }))
    .await?;
    Ok(0)
}

async fn push(ctx: &Ctx<'_>, pick: Pick, git_args: &[String], no_fetch: bool) -> Result<u8> {
    // Find the change request first, so a push never happens without one.
    let number = ctx.number(pick).await?;
    let mut args = vec!["push".to_owned()];
    args.extend(git_args.iter().cloned());
    ctx.git.passthrough(&args).await?;
    let cr = ctx.show(number).await?;
    let (facts, cr) = ctx.report_facts(&cr, no_fetch).await?;
    let head = facts["head"].as_str().unwrap_or_default().to_owned();
    let (dependencies, cr) = ctx.sync_dependencies(cr, &head).await?;
    ctx.emit(json!({
        "type": "change_request",
        "action": "push",
        "git_facts": facts,
        "dependencies": dependencies,
        "verdict": cr["verdict"],
        "change_request": cr,
    }))
    .await?;
    Ok(0)
}

async fn wait(
    ctx: &Ctx<'_>,
    pick: Pick,
    until: &[VerdictState],
    timeout: Option<u64>,
    interval: u64,
) -> Result<u8> {
    let number = ctx.number(pick).await?;
    let wanted: Vec<String> = until.iter().map(|s| name(*s)).collect();
    let deadline = timeout.map(|t| tokio::time::Instant::now() + Duration::from_secs(t));
    let mut cancel = ctx.api.cancel();
    loop {
        let verdict = ctx.verdict(number).await?;
        let state = verdict["state"].as_str().unwrap_or_default();
        let reached = wanted.iter().any(|w| w == state);
        let timed_out = !reached && deadline.is_some_and(|d| tokio::time::Instant::now() >= d);
        if reached || timed_out {
            ctx.emit(json!({
                "type": "change_request_verdict",
                "number": number,
                "verdict": verdict,
                "until": wanted,
                "timed_out": timed_out,
            }))
            .await?;
            return Ok(if reached { 0 } else { TIMED_OUT });
        }
        let mut pause = Duration::from_secs(interval);
        if let Some(deadline) = deadline {
            pause = pause.min(deadline.saturating_duration_since(tokio::time::Instant::now()));
        }
        tokio::select! {
            biased;
            _ = cancel.wait_for(Option::is_some) => bail!("cr wait interrupted"),
            _ = tokio::time::sleep(pause) => {}
        }
    }
}

async fn scenarios(ctx: &Ctx<'_>, command: &ScenariosCommand) -> Result<u8> {
    match command {
        ScenariosCommand::Upload { file, pick } => {
            let file = match file {
                Some(file) => file.clone(),
                None => {
                    let top = ctx.git.toplevel().await?;
                    let file = top.join(ctx.git.claims_path().await?.scenarios());
                    if !file.is_file() {
                        bail!(
                            "there is no scenario set at {}, the branch's default diff set; write it there (gs-qa-scenarios) or pass the set file",
                            file.display()
                        );
                    }
                    file
                }
            };
            let shown = file.display().to_string();
            // Read as bytes and send as they are: the digest covers them.
            let text = read_text(&file)?;
            let set = markdown::scenario_set(&text, &shown)?;
            if set.dirty {
                bail!("{shown} covers uncommitted changes (revision ends +dirty); commit them and write the set again");
            }
            let sections = markdown::sections(&text, &set.ids, &shown)?;
            let digest = markdown::sha256_hex(text.as_bytes());
            let number = ctx.number(*pick).await?;
            let (status, body) = ctx
                .api
                .send(
                    Method::POST,
                    &["change-requests", &number.to_string(), "scenario-sets"],
                    &[],
                    Body::Json(json!({"content": text, "sections": sections})),
                )
                .await?;
            let version = data(body, "scenarios upload")?;
            if version["digest"].as_str().is_some_and(|d| d != digest) {
                bail!(
                    "the server stored digest {} for {shown}, whose SHA-256 is {digest}",
                    version["digest"]
                );
            }
            ctx.emit(json!({
                "type": "scenario_set",
                "number": number,
                "file": shown,
                "source": set.source,
                "digest": digest,
                "scenarios": set.ids,
                "version": version["version"],
                "duplicate": status == 200,
                "stored": version,
            }))
            .await?;
            Ok(0)
        }
        ScenariosCommand::List { pick, source } => {
            let number = ctx.number(*pick).await?;
            let mut sets = scenario_sets(ctx, number).await?;
            // The server's baseline rule: the newest accepted diff version, else v1.
            if let Some(versions) = sets["diff"]["versions"]
                .as_array()
                .filter(|v| !v.is_empty())
            {
                let baseline = versions
                    .iter()
                    .filter(|v| v["accepted"] == true)
                    .filter_map(|v| v["version"].as_u64())
                    .max()
                    .unwrap_or(1);
                sets["diff"]["baseline_version"] = json!(baseline);
            }
            let sources: Vec<String> = match source {
                Some(source) => vec![name(*source)],
                None => vec!["intent".into(), "diff".into()],
            };
            ctx.emit(json!({"type": "scenario_sets", "number": number, "sources": sources, "sets": sets}))
                .await?;
            Ok(0)
        }
        ScenariosCommand::Approve { pick, file, digest } => {
            decide(ctx, *pick, file.as_deref(), digest.as_deref(), None).await
        }
        ScenariosCommand::Reject {
            pick,
            file,
            digest,
            note,
        } => decide(ctx, *pick, file.as_deref(), digest.as_deref(), Some(note)).await,
        ScenariosCommand::Diff {
            pick,
            source,
            from,
            to,
        } => {
            let number = ctx.number(*pick).await?;
            // A diff set's weakening is measured from its baseline, so compare
            // from it unless --from is given; the server defaults to the
            // previous version.
            let baseline = match (source, from) {
                (SetSource::Diff, None) => {
                    let diff = &ctx.verdict(number).await?["scenarios"]["diff"];
                    diff["baseline_version"]
                        .as_u64()
                        .filter(|b| to.or(diff["version"].as_u64()).is_some_and(|t| *b < t))
                }
                _ => None,
            };
            let from = from.or(baseline);
            let source = name(*source);
            let query: Vec<_> = [("from", from), ("to", *to)]
                .into_iter()
                .filter_map(|(k, v)| v.map(|v| (k, v.to_string())))
                .collect();
            let body = ctx
                .api
                .get(
                    &[
                        "change-requests",
                        &number.to_string(),
                        "scenario-sets",
                        &source,
                        "compare",
                    ],
                    &query,
                )
                .await?;
            let comparison = data(body, "scenarios diff")?;
            ctx.emit(json!({"type": "scenario_changes", "number": number, "source": source, "comparison": comparison, "from_baseline": baseline.is_some()}))
                .await?;
            Ok(0)
        }
    }
}

/// `GET …/scenario-sets`: the current version and versions, newest first,
/// of the `intent` and `diff` sets.
async fn scenario_sets(ctx: &Ctx<'_>, number: u64) -> Result<Value> {
    let body = ctx
        .api
        .get(
            &["change-requests", &number.to_string(), "scenario-sets"],
            &[],
        )
        .await?;
    data(body, "scenario sets")
}

/// Approve, or with a note reject, an intent set digest: the given one, a
/// file's, or the current intent version's.
async fn decide(
    ctx: &Ctx<'_>,
    pick: Pick,
    file: Option<&Path>,
    digest: Option<&str>,
    note: Option<&str>,
) -> Result<u8> {
    let number = ctx.number(pick).await?;
    let digest = match (digest, file) {
        (Some(digest), _) => {
            if !lowercase_hex(digest, &[64]) {
                bail!("--digest must be 64 lowercase hex characters");
            }
            digest.to_owned()
        }
        (None, Some(file)) => markdown::sha256_hex(
            &std::fs::read(file).with_context(|| format!("could not read {}", file.display()))?,
        ),
        (None, None) => {
            let sets = scenario_sets(ctx, number).await?;
            match sets["intent"]["current"]["digest"].as_str() {
                Some(digest) => digest.to_owned(),
                None if sets["diff"]["current"].is_object() => bail!(
                    "change request #{number} has only a diff set; a diff set is accepted by approving its diff_set_weakened items: goodflows cr review"
                ),
                None => bail!(
                    "change request #{number} has no intent scenario set; upload one first"
                ),
            }
        }
    };
    let (route, body) = match note {
        None => ("approvals", json!({"digest": digest})),
        Some(note) => ("rejections", json!({"digest": digest, "note": note})),
    };
    let (status, response) = ctx
        .api
        .send(
            Method::POST,
            &[
                "change-requests",
                &number.to_string(),
                "scenario-sets",
                "intent",
                route,
            ],
            &[],
            Body::Json(body),
        )
        .await?;
    let decision = data(
        response,
        if note.is_some() {
            "scenarios reject"
        } else {
            "scenarios approve"
        },
    )?;
    ctx.emit(json!({
        "type": "scenario_decision",
        "number": number,
        "digest": digest,
        "decision": decision,
        "repeated": status == 200,
    }))
    .await?;
    Ok(0)
}

async fn upload_run(ctx: &Ctx<'_>, folder: &Path, pick: Pick) -> Result<u8> {
    let run = verify_run::read(folder)?;
    let run_id = run.document["run_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let (status, body) = ctx
        .api
        .send(
            Method::POST,
            &["verify-runs"],
            &[],
            Body::Json(run.document.clone()),
        )
        .await?;
    let mut stored = data(body, "runs upload")?;
    if stored["status"] == "uploading" {
        put_artifacts(folder, &run.artifacts, &stored, ctx.api.cancel()).await?;
        let body = commit_run(ctx, &run_id).await?;
        stored = data(body, "runs upload")?;
    }
    // Show the verdict of the branch's change request when there is one.
    let number = match pick.number {
        Some(number) => Some(number),
        None => match ctx.git.branch().await {
            Some(_) => ctx
                .on_current_branch()
                .await?
                .map(|cr| number_of(&cr))
                .transpose()?,
            None => None,
        },
    };
    let verdict = match number {
        Some(number) => Some(ctx.verdict(number).await?),
        None => None,
    };
    let results = run.document["results"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let count = |r: &str| results.iter().filter(|x| x["result"] == r).count();
    ctx.emit(json!({
        "type": "verify_run",
        "folder": folder.display().to_string(),
        "run_id": run_id,
        "status": stored["status"],
        "duplicate": status == 200,
        "scenario_set": run.document["scenario_set"],
        "commit": run.document["source"]["commit"],
        "dirty": run.document["source"]["dirty"],
        "results": {"passed": count("passed"), "failed": count("failed"), "not_run": count("not_run")},
        "artifacts": run.artifacts.len(),
        "number": number,
        "verdict": verdict,
    }))
    .await?;
    Ok(0)
}

/// `POST verify-runs/:run_id/commit`, retried up to [`COMMIT_RETRIES`]
/// times while the object store is unavailable (`503`), `retry-after`
/// seconds apart (default 1, at most 10).
async fn commit_run(ctx: &Ctx<'_>, run_id: &str) -> Result<Value> {
    let mut cancel = ctx.api.cancel();
    let mut retries = 0;
    loop {
        match ctx
            .api
            .post(&["verify-runs", run_id, "commit"], Body::None)
            .await
        {
            Err(error) if retries < COMMIT_RETRIES => {
                let Some(unavailable) = HttpError::of(&error, 503) else {
                    return Err(error);
                };
                let pause = Duration::from_secs(unavailable.retry_after.unwrap_or(1).min(10));
                retries += 1;
                tokio::select! {
                    biased;
                    _ = cancel.wait_for(Option::is_some) => bail!("runs upload interrupted"),
                    _ = tokio::time::sleep(pause) => {}
                }
            }
            result => return result,
        }
    }
}

/// PUT each artifact to the presigned URL the server returned for it.
async fn put_artifacts(
    folder: &Path,
    artifacts: &[verify_run::Artifact],
    stored: &Value,
    mut cancel: watch::Receiver<Option<i32>>,
) -> Result<()> {
    const INVALID: &str = "runs upload failed: the server returned invalid upload URLs";
    let uploads = stored["uploads"]
        .as_array()
        .or_else(|| stored["artifacts"].as_array())
        .ok_or_else(|| anyhow!(INVALID))?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(600))
        .build()?;
    for artifact in artifacts {
        let upload = uploads
            .iter()
            .find(|u| u["artifact_id"] == artifact.id.as_str())
            .ok_or_else(|| anyhow!(INVALID))?;
        let url = upload["upload_url"]
            .as_str()
            .and_then(|u| reqwest::Url::parse(u).ok())
            .filter(|u| matches!(u.scheme(), "https" | "http"))
            .ok_or_else(|| anyhow!(INVALID))?;
        let mut request = client.put(url);
        for (name, value) in upload["upload_headers"]
            .as_object()
            .ok_or_else(|| anyhow!(INVALID))?
        {
            let value = value.as_str().ok_or_else(|| anyhow!(INVALID))?;
            request = request.header(name.as_str(), value);
        }
        let bytes = std::fs::read(folder.join(&artifact.file))
            .with_context(|| format!("could not read {}", artifact.file.display()))?;
        if markdown::sha256_hex(&bytes) != artifact.sha256 {
            bail!("{} changed while it was uploaded", artifact.file.display());
        }
        let response = tokio::select! {
            response = request.body(bytes).send() => response
                .map_err(|_| anyhow!("runs upload failed: could not reach the object store"))?,
            _ = cancel.wait_for(Option::is_some) => bail!("runs upload interrupted"),
        };
        if !response.status().is_success() {
            bail!(
                "runs upload failed: the object store refused {} (HTTP {})",
                artifact.id,
                response.status().as_u16()
            );
        }
    }
    Ok(())
}

async fn lease(ctx: &Ctx<'_>, command: &LeaseCommand) -> Result<u8> {
    let (action, response) = match command {
        LeaseCommand::Acquire {
            pick,
            base,
            no_fetch,
        } => {
            let number = ctx.number(*pick).await?;
            let head = ctx.git.head().await?;
            let base = match base {
                Some(base) => ctx.git.resolve(base).await?,
                None => {
                    let cr = ctx.show(number).await?;
                    let target = cr["target_branch"]
                        .as_str()
                        .ok_or_else(|| anyhow!("the change request has no target branch"))?;
                    if !no_fetch {
                        ctx.git.fetch_target(target).await?;
                    }
                    ctx.git.target_commit(target).await?
                }
            };
            let body = ctx
                .api
                .post(
                    &["change-requests", &number.to_string(), "merge-lease"],
                    Body::Json(json!({"head": head, "base": base})),
                )
                .await
                .map_err(|e| explain_conflict(e, |errors| lease_conflict(ctx, number, errors)))?;
            ("acquire", body)
        }
        LeaseCommand::Heartbeat { lease_id } => {
            let body = ctx
                .api
                .post(&["merge-leases", lease_id, "heartbeat"], Body::None)
                .await
                .map_err(|e| {
                    explain_conflict(e, |errors| lease_not_active(ctx, lease_id, errors))
                })?;
            ("heartbeat", body)
        }
        LeaseCommand::Release { lease_id } => {
            ctx.api
                .send(Method::DELETE, &["merge-leases", lease_id], &[], Body::None)
                .await
                .map_err(|e| {
                    explain_conflict(e, |errors| lease_not_active(ctx, lease_id, errors))
                })?;
            (
                "release",
                json!({"data": {"id": lease_id, "state": "released"}}),
            )
        }
    };
    let lease = data(response, "lease")?;
    ctx.emit(json!({"type": "merge_lease", "action": action, "lease": lease}))
        .await?;
    Ok(0)
}

/// The message for a coded `409` of `POST .../merge-lease`.
fn lease_conflict(ctx: &Ctx<'_>, number: u64, errors: &Value) -> Option<String> {
    let command = ctx.api.command();
    let reason = match errors["error"].as_str()? {
        "slot_taken" => match errors.get("holder").filter(|h| h.is_object()) {
            Some(holder) => format!(
                "another lease holds the merge slot of the target branch: change request #{} holds it until {}; wait for it to merge or expire, then acquire again",
                quoted(&holder["change_request"]),
                quoted(&holder["expires_at"])
            ),
            None => "another lease holds the merge slot of the target branch; acquire again".to_owned(),
        },
        "not_mergeable" => format!(
            "change request #{number} is not mergeable (verdict {}); see goodflows cr verdict",
            quoted(&errors["state"])
        ),
        "stale_head" => format!(
            "HEAD is not the reported head of change request #{number}, which is {}; run goodflows cr push or goodflows cr claims, or check out that head",
            quoted(&errors["head"])
        ),
        "not_open" => format!(
            "change request #{number} is not open; it must be ready first (goodflows cr ready)"
        ),
        _ => return None,
    };
    Some(format!("{command} failed: {reason}"))
}

/// The message for a `lease_not_active` `409` of heartbeat and release.
fn lease_not_active(ctx: &Ctx<'_>, lease_id: &str, errors: &Value) -> Option<String> {
    (errors["error"] == "lease_not_active").then(|| {
        format!(
            "{} failed: lease {} is no longer active (unknown, released or expired); acquire a new one with goodflows cr lease acquire",
            ctx.api.command(),
            crate::output::visible(lease_id)
        )
    })
}

pub async fn policy(
    reporting: Option<&Reporting>,
    command: &PolicyCommand,
    output: &Output,
    cancel: watch::Receiver<Option<i32>>,
) -> Result<u8> {
    let label = match command {
        PolicyCommand::Get { .. } => "policy get",
        PolicyCommand::Set { .. } => "policy set",
        PolicyCommand::History { .. } => "policy history",
        PolicyCommand::Calibration { .. } => "policy calibration",
    };
    let ctx = Ctx::new(reporting, label, output, cancel)?;
    match command {
        PolicyCommand::Get { version, effective } => {
            let body = match version {
                Some(version) => {
                    ctx.api
                        .get(&["review-policy", "versions", &version.to_string()], &[])
                        .await?
                }
                None => ctx.api.get(&["review-policy"], &[]).await?,
            };
            let policy = data(body, label)?;
            ctx.emit(json!({"type": "review_policy", "policy": policy, "effective": effective}))
                .await?;
        }
        PolicyCommand::Set {
            file,
            previous_version,
            note,
        } => {
            let text = read_text(file)?;
            let document: toml::Value = toml::from_str(&text)
                .map_err(|e| anyhow!("{} is not valid TOML: {e}", file.display()))?;
            let mut document = serde_json::to_value(document)?;
            if document.get("format").is_none() {
                document["format"] = json!("review-policy/1");
            }
            let previous = match previous_version {
                Some(previous) => *previous,
                None => {
                    let current = data(ctx.api.get(&["review-policy"], &[]).await?, label)?;
                    // Writing the current document again would only add an
                    // identical version.
                    let same_note = note.as_ref().is_none_or(|n| current["note"] == n.as_str());
                    if current["policy"] == document && same_note {
                        ctx.emit(json!({"type": "review_policy", "policy": current, "written": false, "effective": false}))
                            .await?;
                        return Ok(0);
                    }
                    current["version"]
                        .as_u64()
                        .ok_or_else(|| anyhow!("the server's policy has no version"))?
                }
            };
            let mut body = json!({"previous_version": previous, "policy": document});
            if let Some(note) = note {
                body["note"] = json!(note);
            }
            let (status, response) = ctx
                .api
                .send(Method::PUT, &["review-policy"], &[], Body::Json(body))
                .await
                .map_err(|error| {
                    explain_conflict(error, |errors| {
                        (errors["detail"] == POLICY_CONFLICT).then(|| {
                            format!(
                                "{label} failed: version {previous} is no longer the current policy version, because another write landed first; read it with goodflows policy get, make the change again, and set it"
                            )
                        })
                    })
                })?;
            let policy = data(response, label)?;
            ctx.emit(json!({"type": "review_policy", "policy": policy, "written": status == 201, "effective": false}))
                .await?;
        }
        PolicyCommand::History { limit, cursor } => {
            let query: Vec<_> = [
                ("limit", limit.map(|l| l.to_string())),
                ("cursor", cursor.clone()),
            ]
            .into_iter()
            .filter_map(|(k, v)| v.map(|v| (k, v)))
            .collect();
            let body = ctx.api.get(&["review-policy", "versions"], &query).await?;
            let versions = body["data"].clone();
            if !versions.is_array() {
                bail!("{label} failed: the server's response is not a list of versions");
            }
            ctx.emit(json!({"type": "review_policy_versions", "versions": versions, "next_cursor": body["next_cursor"]}))
                .await?;
        }
        PolicyCommand::Calibration { from, to } => {
            let query: Vec<_> = [("from", from.clone()), ("to", to.clone())]
                .into_iter()
                .filter_map(|(k, v)| v.map(|v| (k, v)))
                .collect();
            let body = ctx.api.get(&["calibration"], &query).await?;
            let report = data(body, label)?;
            ctx.emit(json!({"type": "calibration", "report": report}))
                .await?;
        }
    }
    Ok(0)
}

pub async fn features(
    reporting: Option<&Reporting>,
    command: &FeaturesCommand,
    output: &Output,
    cancel: watch::Receiver<Option<i32>>,
) -> Result<u8> {
    let token_env = reporting.map_or("GOODFLOWS_API_KEY", |r| r.token_env.as_str());
    let dir = match command {
        FeaturesCommand::Sync { dir } | FeaturesCommand::Validate { dir } => match dir {
            Some(dir) => dir.clone(),
            None => features_dir(&Git::new(token_env).toplevel().await?)?,
        },
    };
    let read = read_features(&dir)?;
    let errors = feature_dir_problems(&dir, &read);
    if let FeaturesCommand::Validate { .. } = command {
        return validate_features(&dir, read, errors, output).await;
    }
    // Needs the server, unlike validate.
    let ctx = Ctx::new(reporting, "features sync", output, cancel)?;
    let mut problems = Problems(errors);
    let mut features = Vec::new();
    let mut skipped = Vec::new();
    for (_, feature) in read {
        match feature {
            Ok(feature) if feature.paths.is_empty() => skipped.push(feature.id),
            Ok(feature) => features
                .push(json!({"id": feature.id, "title": feature.title, "paths": feature.paths})),
            Err(errors) => problems.0.extend(errors),
        }
    }
    problems.check()?;
    let body = ctx
        .api
        .post(
            &["feature-index"],
            Body::Json(json!({"features": features})),
        )
        .await?;
    let index = data(body, "features sync")?;
    ctx.emit(json!({
        "type": "feature_index",
        "dir": dir.display().to_string(),
        "version": index["version"],
        "features": features.iter().map(|f| f["id"].clone()).collect::<Vec<_>>(),
        "skipped": skipped,
    }))
    .await?;
    Ok(0)
}

/// Each feature file of `dir`, every `.md` but README, with what it reads
/// as or its problems.
type FeatureFiles = Vec<(PathBuf, Result<markdown::Feature, Vec<String>>)>;

fn read_features(dir: &Path) -> Result<FeatureFiles> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("could not read {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.extension().is_some_and(|e| e == "md")
                && !path
                    .file_stem()
                    .is_some_and(|s| s.eq_ignore_ascii_case("readme"))
                && path.is_file()
        })
        .collect();
    files.sort();
    Ok(files
        .into_iter()
        .map(|file| {
            let shown = file.display().to_string();
            let stem = file
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let feature = read_text(&file)
                .and_then(|text| markdown::feature(&text, &stem, &shown))
                .map_err(markdown::problems);
            (file, feature)
        })
        .collect())
}

/// Problems of the directory as a whole: an id that several files with
/// paths share, which the index could hold only once, and no file with
/// paths, which leaves nothing to sync.
fn feature_dir_problems(dir: &Path, files: &FeatureFiles) -> Vec<String> {
    let mut by_id: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (file, feature) in files {
        if let Some(feature) = feature.as_ref().ok().filter(|f| !f.paths.is_empty()) {
            by_id
                .entry(&feature.id)
                .or_default()
                .push(file.display().to_string());
        }
    }
    let mut problems: Vec<String> = by_id
        .iter()
        .filter(|(_, files)| files.len() > 1)
        .map(|(id, files)| {
            format!(
                "feature id {id} is used by {}; give each file its own id",
                files.join(" and ")
            )
        })
        .collect();
    if by_id.is_empty() {
        problems.push(format!(
            "no feature file in {} has paths globs in its front matter; nothing to sync",
            dir.display()
        ));
    }
    problems
}

async fn validate_features(
    dir: &Path,
    read: FeatureFiles,
    errors: Vec<String>,
    output: &Output,
) -> Result<u8> {
    let mut ok = errors.is_empty();
    let mut features = Vec::new();
    let mut skipped = Vec::new();
    let mut files = Vec::new();
    for (file, feature) in read {
        let file = file.display().to_string();
        files.push(match feature {
            Ok(feature) => {
                let mut warnings = Vec::new();
                if feature.paths.is_empty() {
                    warnings.push(format!(
                        "{file}: no paths globs in the front matter, so features sync skips it"
                    ));
                    skipped.push(feature.id.clone());
                } else {
                    features.push(feature.id.clone());
                }
                json!({"file": file, "id": feature.id, "ok": true, "errors": [], "warnings": warnings})
            }
            Err(errors) => {
                ok = false;
                json!({"file": file, "id": null, "ok": false, "errors": errors, "warnings": []})
            }
        });
    }
    output
        .emit(json!({
            "type": "feature_validation",
            "dir": dir.display().to_string(),
            "ok": ok,
            "features": features,
            "skipped": skipped,
            "files": files,
            "errors": errors,
        }))
        .await?;
    Ok(if ok { 0 } else { 1 })
}

/// The one `verify-*/features` directory in the checkout's skills directories.
fn features_dir(toplevel: &Path) -> Result<PathBuf> {
    let mut found = Vec::new();
    for skills in [".agents/skills", ".claude/skills", ".cursor/skills"] {
        let Ok(entries) = std::fs::read_dir(toplevel.join(skills)) else {
            continue;
        };
        for entry in entries.flatten() {
            let features = entry.path().join("features");
            if entry.file_name().to_string_lossy().starts_with("verify-") && features.is_dir() {
                found.push(features);
            }
        }
    }
    found.sort();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => bail!("found no verify-*/features directory in .agents/skills, .claude/skills or .cursor/skills; pass --dir"),
        _ => bail!(
            "found several feature directories ({}); pass --dir",
            found
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub async fn events(
    reporting: Option<&Reporting>,
    args: &EventsArgs,
    output: &Output,
    cancel: watch::Receiver<Option<i32>>,
) -> Result<u8> {
    let ctx = Ctx::new(reporting, "events", output, cancel)?;
    let mut after = args.after;
    loop {
        let wait = args.wait.or(args.follow.then_some(30));
        let query: Vec<_> = [
            ("after", after.map(|a| a.to_string())),
            ("limit", args.limit.map(|l| l.to_string())),
            ("wait", wait.map(|w| w.to_string())),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| (k, v)))
        .collect();
        let body = match ctx.api.get(&["events"], &query).await {
            Ok(body) => body,
            Err(error) => {
                let Some(gone) = HttpError::of(&error, 410) else {
                    return Err(error);
                };
                let Some(latest) = gone.body["latest_cursor"].as_u64() else {
                    return Err(error);
                };
                if !args.follow {
                    bail!(
                        "events failed: the events after cursor {} were deleted by retention; resync from current state, then read with --after {latest} (latest_cursor)",
                        after.map_or("(none)".to_owned(), |a| a.to_string())
                    );
                }
                let note = format!(
                    "events after cursor {} were deleted by retention and skipped; resync from current state; continuing after {latest}",
                    after.map_or("(none)".to_owned(), |a| a.to_string())
                );
                ctx.emit(json!({
                    "type": "project_events",
                    "events": [],
                    "next_cursor": latest,
                    "skipped": true,
                    "note": note,
                }))
                .await?;
                after = Some(latest);
                continue;
            }
        };
        let events = &body["data"];
        if !events.is_array() {
            bail!("events failed: the server's response is not a list of events");
        }
        if !args.follow || !events.as_array().is_some_and(Vec::is_empty) {
            ctx.emit(json!({"type": "project_events", "events": events, "next_cursor": body["next_cursor"]}))
                .await?;
        }
        if !args.follow {
            return Ok(0);
        }
        if let Some(next) = body["next_cursor"].as_u64() {
            after = Some(next);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_ids_follow_the_server_rule() {
        assert!(change_id("add-login").is_ok());
        assert!(change_id("2fa").is_ok());
        for bad in ["", "-x", "Add", "a_b", &"a".repeat(65)] {
            assert!(change_id(bad).is_err(), "{bad}");
        }
    }
}
