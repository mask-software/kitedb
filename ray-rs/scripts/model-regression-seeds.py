#!/usr/bin/env python3
"""Derive the model test's pinned regression seeds.

`REGRESSION_SEEDS` in src/core/single_file/b4_checkpoint_model_tests.rs pins
seeds that catch the bugs reviews found in WAL segments and checkpoints. A
seed catches a bug only through what the model's random draws make it do, so
any change to the model moves the catches: re-derive the list then.

For each mutant below (a bug re-made in the code), this applies it, runs the
model test's quick run on seeds [--first, --first + --seeds) --repeats times,
and restores the source (also on an error or Ctrl-C). The seeds that failed
on every run are candidates, those whose failure shows no checkpoint thread
and no application checkpointer first (those depend on timing too). It pins
the first --pin candidates of each mutant, then runs the pinned seeds
together, as the regression test does, --repeats times under each mutant,
and replaces a pinned seed that ever passed with the next candidate, until
every pinned seed catches its mutant on every run. It prints the list to
paste; a mutant left with no seed needs a wider --seeds.

When a mutant's text no longer matches the source (the code moved on), the
script stops: update the mutant, keeping it the bug it names, and its line
in the list's doc comment.

    python3 scripts/model-regression-seeds.py [--seeds 300] [--repeats 3]
        [--pin 2] [--mutant NAME ...]
"""

import argparse
import os
import re
import signal
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TEST = "core::single_file::checkpoint::b4_checkpoint_model_tests::wal_segment_and_checkpoint_model"

# name -> (what it re-makes, [(file, text, mutated text)])
MUTANTS = {
    "r7": (
        "open forgets the transactions whose records a spill moved (R7)",
        [(
            "src/core/single_file/open.rs",
            "    spilled_txids: Mutex::new(spilled_txids),",
            "    spilled_txids: Mutex::new({ let _ = spilled_txids; HashMap::new() }),",
        )],
    ),
    "f3-ok": (
        "a cut with the segment table full covers nothing, and returns Ok (F3)",
        [(
            "src/core/single_file/checkpoint.rs",
            """        wal_left = true;
        #[cfg(test)]
        count_checkpoint_test_cover_without_spill(&self.path);
      }""",
            """        wal_left = true;
        #[cfg(test)]
        count_checkpoint_test_cover_without_spill(&self.path);
        self.drop_unneeded_wal_segments(&mut pager, &mut header, &unneeded)?;
        return Ok(CutOutcome::NothingToCover);
      }""",
        )],
    ),
    "f3-declined": (
        "a cut with the segment table full covers nothing, and declines (F3)",
        [(
            "src/core/single_file/checkpoint.rs",
            """        wal_left = true;
        #[cfg(test)]
        count_checkpoint_test_cover_without_spill(&self.path);
      }""",
            """        wal_left = true;
        #[cfg(test)]
        count_checkpoint_test_cover_without_spill(&self.path);
        self.drop_unneeded_wal_segments(&mut pager, &mut header, &unneeded)?;
        return Ok(CutOutcome::Blocked);
      }""",
        )],
    ),
    "needed-after": (
        "wal_segments_needed_after ignores the transactions that commit after the cut",
        [(
            "src/core/single_file/segments.rs",
            "      .filter(|spilled| spilled.commit_segment.is_none_or(|commit| commit > covered))",
            "      .filter(|spilled| spilled.commit_segment.is_none())",
        )],
    ),
    "unsealed": (
        "a cut does not seal the newest segment",
        [(
            "src/core/single_file/checkpoint.rs",
            "    Self::seal_newest_wal_segment(&mut header);\n",
            "    let _ = Self::seal_newest_wal_segment;\n",
        )],
    ),
    "forget-spilled": (
        "an install forgets every spilled transaction",
        [(
            "src/core/single_file/segments.rs",
            "    self.spilled_txids.lock().retain(|_, spilled| {",
            "    self.spilled_txids.lock().retain(|_, spilled| false && {",
        )],
    ),
    "r12": (
        "a header page's footer checksum covers its fixed fields' checksum, so not them (R12)",
        [
            (
                "src/core/header.rs",
                """  crc32_multi(&[
    &page[..HEADER_CRC_OFFSET],
    &page[HEADER_CRC_END..page_size - 4],
  ])""",
                """  let _ = crc32_multi;
  crc32(&page[..page_size - 4])""",
            ),
            (
                "src/core/header.rs",
                "      &[&buf[..HEADER_CRC_OFFSET], &buf[HEADER_CRC_END..table_end]],",
                "      &[&buf[..table_end]],",
            ),
        ],
    ),
    "spill-slot": (
        "a spill does not sync its second header slot",
        [(
            "src/core/single_file/segments.rs",
            """    checkpoint_phase(&self.path, CheckpointPhase::SpillHeaderDurable)?;
    self.persist_header(pager, header, true)""",
            """    checkpoint_phase(&self.path, CheckpointPhase::SpillHeaderDurable)?;
    self.persist_header(pager, header, false)""",
        )],
    ),
}


def apply(edits):
    """Apply `edits`; returns the original texts by file."""
    texts = {}
    for path, old, new in edits:
        full = os.path.join(ROOT, path)
        text = texts.get(path) or open(full).read()
        if text.count(old) != 1:
            sys.exit(f"{path}: the mutant's text is there {text.count(old)} times, not once")
        texts[path] = text.replace(old, new)
    originals = {path: open(os.path.join(ROOT, path)).read() for path in texts}
    for path, text in texts.items():
        open(os.path.join(ROOT, path), "w").write(text)
    return originals


def restore(originals):
    for path, text in originals.items():
        open(os.path.join(ROOT, path), "w").write(text)


def run_model(env_seeds):
    """Run the quick model test over `env_seeds` (environment variables);
    returns each failing seed's failure text."""
    env = dict(os.environ, KITE_MODEL_SEED_TIMEOUT="60", CARGO_PROFILE_DEV_DEBUG="0")
    for name in ("KITE_MODEL_SEED", "KITE_MODEL_SEEDS", "KITE_MODEL_FIRST_SEED"):
        env.pop(name, None)
    env.update(env_seeds)
    run = subprocess.run(
        ["cargo", "test", "--lib", TEST, "--", "--exact", "--nocapture"],
        cwd=ROOT, env=env, capture_output=True, text=True)
    output = run.stdout + run.stderr
    if "model test:" not in output:
        sys.exit("the model test did not run:\n" + output[-4000:])
    failures = {}
    for match in re.finditer(r"^model test: seed (\d+) (.*?)(?=^model test: |\Z)", output,
                             re.M | re.S):
        failures[int(match.group(1))] = match.group(2)
    return failures


def timing_free(failure):
    """Whether a failure's options and steps show no checkpoint thread and no
    application checkpointer."""
    return "thread: false" in failure and "application checkpointer" not in failure


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--first", type=int, default=0)
    parser.add_argument("--seeds", type=int, default=300)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--pin", type=int, default=2)
    parser.add_argument("--mutant", action="append", choices=sorted(MUTANTS))
    args = parser.parse_args()
    signal.signal(signal.SIGINT, signal.default_int_handler)
    names = args.mutant or list(MUTANTS)

    candidates = {}
    for name in names:
        what, edits = MUTANTS[name]
        originals = apply(edits)
        try:
            runs = [run_model({"KITE_MODEL_FIRST_SEED": str(args.first),
                               "KITE_MODEL_SEEDS": str(args.seeds)})
                    for _ in range(args.repeats)]
        finally:
            restore(originals)
        always = sorted(set.intersection(*(set(run) for run in runs)))
        sometimes = sorted(set.union(*(set(run) for run in runs)) - set(always))
        free = [seed for seed in always if all(timing_free(run[seed]) for run in runs)]
        candidates[name] = free + [seed for seed in always if seed not in free]
        print(f"{name} ({what}): caught on every run by {len(always)} of {args.seeds} seeds "
              f"{always[:20]}, {len(free)} of them free of timing; on some runs by "
              f"{sometimes[:20]}", flush=True)

    pinned = {name: candidates[name][:args.pin] for name in names}
    while True:
        seeds = sorted({seed for chosen in pinned.values() for seed in chosen})
        dropped = False
        for name in names:
            if not pinned[name]:
                continue
            originals = apply(MUTANTS[name][1])
            try:
                runs = [run_model({"KITE_MODEL_SEED": ",".join(map(str, seeds))})
                        for _ in range(args.repeats)]
            finally:
                restore(originals)
            missed = [seed for seed in pinned[name] if any(seed not in run for run in runs)]
            for seed in missed:
                print(f"{name}: seed {seed} passed among the pinned seeds; replacing it",
                      flush=True)
                candidates[name].remove(seed)
                dropped = True
            pinned[name] = candidates[name][:args.pin]
        if not dropped:
            break

    print("\nconst REGRESSION_SEEDS: &[(u64, &str)] = &[")
    for name in names:
        if not pinned[name]:
            print(f"  // {name}: no seed in [{args.first}, {args.first + args.seeds}) catches it")
        for seed in pinned[name]:
            print(f'  ({seed}, "{name}"),')
    print("];")


if __name__ == "__main__":
    main()
