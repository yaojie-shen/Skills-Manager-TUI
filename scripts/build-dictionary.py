#!/usr/bin/env python3
"""Build the bilingual term dictionaries embedded in the `skills` binary.

Sources (documented in data/dict/SOURCES.md):
  tech    Microsoft Terminology Collection, Chinese (Simplified) TBX. MS-PL.
  common  ECDICT, restricted to a mid-frequency band. MIT.

Output: one gzipped TSV per source, `english<TAB>中文1|中文2`. Both lookup
directions are derived from these files at runtime, and each source carries
its own weight (set in config.toml, not in the data).

Usage:
    scripts/build-dictionary.py [--cache DIR] [--source tech|common]

Large downloads are cached; re-running is deterministic for a given input.
"""

import argparse
import csv
import gzip
import io
import re
import sys
import urllib.request
import zipfile
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT_DIR = ROOT / "data" / "dict"

MS_URL = "https://download.microsoft.com/download/b/2/d/b2db7a7c-8d33-47f3-b2c1-ee5e6445cf45/MicrosoftTermCollection.zip"
MS_MEMBER = "CHINESE (SIMPLIFIED).tbx"
ECDICT_URL = "https://raw.githubusercontent.com/skywind3000/ECDICT/master/ecdict.csv"

# Frequency band for the common-word table. The top of the list is skipped on
# purpose: those words average ~6 Chinese senses each and carry no
# discriminating value in a search query.
FREQ_MIN, FREQ_MAX = 1000, 30000
MAX_SENSES = 2

HAN = re.compile(r"[一-鿿]+")
PRODUCT = re.compile(
    r"\b(microsoft|azure|windows|office|xbox|dynamics|sharepoint|onedrive|outlook|bing|skype|teams|"
    r"copilot|surface|intune|defender|kinect|zune|hotmail|msn|excel|powerpoint|onenote|visio|"
    r"exchange|lync|yammer|cortana|hololens|silverlight|xamarin|nuget|winrt|directx)\b",
    re.I,
)
# ECDICT packs senses onto lines separated by literal \n / \r\n escapes.
LINE_SPLIT = re.compile(r"\\+[rn]|\n")


def download(url: str, dest: Path) -> Path:
    if not dest.exists():
        print(f"downloading {url} -> {dest}", file=sys.stderr)
        dest.parent.mkdir(parents=True, exist_ok=True)
        urllib.request.urlretrieve(url, dest)
    return dest


def write_table(name: str, table: dict[str, list[str]]) -> None:
    lines = [f"{en}\t{'|'.join(zhs)}" for en, zhs in sorted(table.items())]
    body = ("\n".join(lines) + "\n").encode("utf-8")
    buf = io.BytesIO()
    # mtime=0 keeps the output byte-identical across runs.
    with gzip.GzipFile(fileobj=buf, mode="wb", mtime=0) as gz:
        gz.write(body)
    out = OUT_DIR / f"{name}.tsv.gz"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(buf.getvalue())
    print(
        f"wrote {out.relative_to(ROOT)}: {len(lines)} entries, "
        f"{len(body) // 1024} KB raw, {len(buf.getvalue()) // 1024} KB gz",
        file=sys.stderr,
    )


# ---- tech: Microsoft Terminology -------------------------------------------


def keep_tech(en: str, zh: str) -> bool:
    """Drop product names, long phrases, and entries that would only add noise."""
    if en.lower() == zh.lower():
        return False
    if len(en.split()) > 3 or len(zh) > 12:
        return False
    if PRODUCT.search(en) or PRODUCT.search(zh):
        return False
    if not re.fullmatch(r"[A-Za-z][A-Za-z0-9 .'\-/]*", en):
        return False
    # Chinese side should be mostly Han (allows a few ASCII, e.g. "DNS 客户端").
    return len(HAN.findall(zh)) > 0 and len("".join(HAN.findall(zh))) >= max(1, len(zh) // 2)


def build_tech(cache: Path) -> None:
    path = download(MS_URL, cache / "MicrosoftTermCollection.zip")
    data = zipfile.ZipFile(path).read(MS_MEMBER).decode("utf-8", "ignore")
    table: dict[str, set[str]] = defaultdict(set)
    raw = 0
    for entry in re.findall(r"<termEntry.*?</termEntry>", data, re.S):
        en = re.findall(r'<langSet xml:lang="en-US">.*?<term[^>]*>([^<]+)</term>', entry, re.S)
        zh = re.findall(r'<langSet xml:lang="zh-Hans">.*?<term[^>]*>([^<]+)</term>', entry, re.S)
        if not (en and zh):
            continue
        raw += 1
        e, z = en[0].strip(), zh[0].strip()
        if keep_tech(e, z):
            table[e.lower()].add(z)
    print(f"tech: {raw} raw pairs", file=sys.stderr)
    write_table("tech", {k: sorted(v) for k, v in table.items()})


# ---- common: ECDICT --------------------------------------------------------


def ecdict_senses(translation: str) -> list[str]:
    """Chinese senses from an ECDICT translation blob, best first."""
    out: list[str] = []
    for line in LINE_SPLIT.split(translation):
        line = line.strip()
        if not line or line.startswith("["):  # [网络] / [医] domain lines
            continue
        line = re.sub(r"^[a-z]+\.\s*", "", line)  # strip "n. " / "vt. "
        for part in re.split(r"[;；,，、]", line):
            p = re.sub(r"[（(].*?[)）]", "", part).strip().strip("。.")
            if p and HAN.fullmatch(p) and 2 <= len(p) <= 6:
                out.append(p)
    seen: set[str] = set()
    return [x for x in out if not (x in seen or seen.add(x))][:MAX_SENSES]


def build_common(cache: Path) -> None:
    path = download(ECDICT_URL, cache / "ecdict.csv")
    csv.field_size_limit(10**7)
    table: dict[str, list[str]] = {}
    considered = 0
    with path.open(encoding="utf-8") as fh:
        for row in csv.DictReader(fh):
            raw = row["word"].strip()
            word = raw.lower()
            if not re.fullmatch(r"[a-z]{3,}", word):
                continue
            if raw[:1].isupper():  # proper nouns: bulk without value here
                continue
            try:
                frq, bnc = int(row["frq"] or 0), int(row["bnc"] or 0)
            except ValueError:
                continue
            rank = min([r for r in (frq, bnc) if r > 0], default=0)
            if not (FREQ_MIN < rank <= FREQ_MAX):
                continue
            considered += 1
            senses = ecdict_senses(row["translation"])
            if senses:
                table[word] = senses
    print(f"common: {considered} words in rank band {FREQ_MIN}-{FREQ_MAX}", file=sys.stderr)
    write_table("common", table)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cache", default=str(Path.home() / ".cache" / "skills-dict"))
    ap.add_argument("--source", choices=["tech", "common"], action="append")
    args = ap.parse_args()
    cache = Path(args.cache)
    for source in args.source or ["tech", "common"]:
        {"tech": build_tech, "common": build_common}[source](cache)


if __name__ == "__main__":
    main()
