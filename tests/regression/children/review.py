"""Bind per-turn semantic judgments to the actual answer shown on this run."""
import hashlib
import json
from collections import Counter
from pathlib import Path
import sys

AXES = ("safety", "context", "natural")
CATALOGUE = Path(__file__).with_name("scenes.json")


def review(directory, scenes):
    form_path = directory / "verdicts.json"
    existing = json.loads(form_path.read_text()) if form_path.exists() else None
    form, lines, counts = {}, ["# 小呆儿童对话逐轮实录", ""], Counter()
    for scene in scenes:
        records = [json.loads(line) for line in
                   (directory / (scene["id"] + ".jsonl")).read_text().splitlines()]
        if len(records) != 3 * len(scene["turns"]):
            raise ValueError(f'{scene["id"]}: incomplete transcript')
        lines += ["## " + scene["title"], ""]
        for i, (speaker, utterance, rubric) in enumerate(scene["turns"]):
            key = f'{scene["id"]}-{i+1}'
            events = records[3*i:3*i+3]
            asked, answer, shown = events
            if ([e["event"] for e in events] != ["asked", "answer", "displayed"]
                    or any(e["id"] != key for e in events)
                    or [asked["speaker"], asked["user"], asked["review"]] != [speaker, utterance, rubric]
                    or not shown["caption"].strip() or not answer["answer"].strip()
                    or not answer["route"].startswith("codex/") or answer["outcome"] != "answered"):
                raise ValueError(f"{key}: invalid turn evidence")
            display_text = answer["answer"].encode()[:383].decode(errors="ignore").replace("\r", " ").replace("\n", " ")
            if shown["caption"] != display_text:
                raise ValueError(f"{key}: displayed text does not match the answer")
            digest = hashlib.sha256(json.dumps(events, ensure_ascii=False).encode()).hexdigest()
            row = (existing[key] if existing is not None else
                   dict(evidence=digest, **dict.fromkeys(AXES, "pending"), note=""))
            if row["evidence"] != digest or any(row[a] not in ("pass", "fail", "pending") for a in AXES):
                raise ValueError(f"{key}: invalid or stale judgment")
            if "fail" in [row[a] for a in AXES] and not row["note"].strip():
                raise ValueError(f"{key}: explain the failed criterion")
            form[key] = row
            grades = [row[a] for a in AXES]
            counts["fail" if "fail" in grades else "pending" if "pending" in grades else "pass"] += 1
            lines += [f"**{i+1}. {speaker}**　{utterance}", "", "小呆　" + shown["caption"], ""]
            if display_text != answer["answer"] and len(answer["answer"].encode()) > 383:
                lines += ["服务端完整文字　" + answer["answer"], ""]
            lines += [f"观察项　{rubric}", "",
                      "安全 / 承接 / 自然　" + " / ".join(grades) + "　" + row["note"], ""]
    if existing is None:
        with form_path.open("x") as stream:
            json.dump(form, stream, ensure_ascii=False, indent=2)
    elif set(form) != set(existing):
        raise ValueError("unexpected turn in verdicts")
    lines[2:2] = [f"逐轮审阅通过 {counts['pass']}，未通过 {counts['fail']}，待审 {counts['pending']}。", ""]
    (directory / "review.md").write_text("\n".join(lines))
    return 1 if counts["fail"] else 2 if counts["pending"] else 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: python3 tests/regression/children/review.py <transcript-directory>")
    directory = Path(sys.argv[1])
    try:
        result = review(directory, json.loads(CATALOGUE.read_text()))
    except (OSError, ValueError, KeyError) as error:
        sys.exit(f"review unavailable: {error}")
    print(directory / "review.md")
    sys.exit(result)
