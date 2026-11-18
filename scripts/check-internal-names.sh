#!/usr/bin/env bash
# Usage: scripts/check-internal-names.sh [repo-dir]       every tracked or untracked (not ignored) text file
#        scripts/check-internal-names.sh --staged          only the lines the staged diff adds (pre-commit)
#        scripts/check-internal-names.sh --message FILE    a commit message (commit-msg)
# Fails when the text names internal infrastructure. Generic rules, always on:
#   - a host in an internal `.syno` domain (lowercase, such as <host>.syno);
#   - a company-internal or personal domain;
#   - a work-thread id (t- and four digits);
#   - a macOS home folder (/Users/<name>);
#   - a private key block or a GitHub token;
#   - a private IPv4 address (10/8, 172.16/12, 192.168/16) other than the common
#     example addresses (192.168.0-2.x, 10.0.0.x, 10.1.2.3, 10.2.3.4, and the
#     QEMU and Docker defaults 10.0.2.2 and 172.17.0.1).
# Machine names are never written into the repository. To check them too, list
# them outside it, comma- or space-separated in SYNOPKGLAND_INTERNAL_NAMES or one
# per line (# comments allowed) in the file SYNOPKGLAND_INTERNAL_NAMES_FILE;
# without either, that rule is skipped. Matching is case-insensitive on word
# boundaries. The same file is kept in each App repository; only the settings
# below differ.
set -euo pipefail

# Settings for this repository.
SKIP='' # path prefixes, space-separated, not checked (upstream code and data)

mode=tree
case ${1:-} in
  --staged) mode=staged ;;
  --message) mode=message; msg=${2:?--message needs a file}; [[ $msg = /* ]] || msg=$PWD/$msg ;;
  *) cd "${1:-$(dirname "$0")/..}" ;;
esac
[[ $mode = tree ]] || cd "$(git rev-parse --show-toplevel)"

names=${SYNOPKGLAND_INTERNAL_NAMES:-${SYNORECLAIM_INTERNAL_NAMES:-}}
file=${SYNOPKGLAND_INTERNAL_NAMES_FILE:-${SYNORECLAIM_INTERNAL_NAMES_FILE:-}}
if [[ -n $file ]]; then
  [[ -r $file ]] || { echo "check-internal-names: cannot read $file" >&2; exit 2; }
  names+=" $(sed -e 's/#.*//' "$file" | tr '\n' ' ')"
fi
if [[ $mode = tree && -z ${names//[[:space:],]/} ]]; then
  echo "check-internal-names: no machine-name list given; checking the generic rules only"
fi

# Every input becomes "path<TAB>line<TAB>text" records for one rule pass.
records() {
  case $mode in
    tree) git ls-files -z --cached --others --exclude-standard | perl -0 -ne '
      chomp(my $p = $_); next unless -f $p && !-l $p && -T $p;
      open(my $fh, "<", $p) or next; local $/ = "\n";
      while (my $l = <$fh>) { $l =~ s/[\r\n]+$//; $l =~ s/\t/ /g; print "$p\t$.\t$l\n"; }' ;;
    staged) git diff --cached -U0 --no-color --no-ext-diff --diff-filter=ACMR | perl -ne '
      if (/^\+\+\+ (?:b\/)?(.*)$/) { $p = $1; next } if (/^@@ -\S+ \+(\d+)/) { $n = $1 - 1; next }
      next unless /^\+/; $n++; s/^\+//; s/[\r\n]+$//; s/\t/ /g; print "$p\t$n\t$_\n";' ;;
    message) git stripspace --strip-comments <"$msg" | perl -ne 's/[\r\n]+$//; s/\t/ /g; print "commit message\t$.\t$_\n";' ;;
  esac
}

records | INTERNAL_NAMES=$names SKIP=$SKIP perl -ne '
  BEGIN {
    my @names = grep { length } split /[\s,]+/, ($ENV{INTERNAL_NAMES} // "");
    $names = @names ? join("|", map { quotemeta } sort { length($b) <=> length($a) } @names) : "";
    @skip = grep { length } split / +/, $ENV{SKIP}; $found = 0;
  }
  chomp; my ($path, $n, $line) = split /\t/, $_, 3; $line //= "";
  next if grep { index($path, $_) == 0 } @skip;
  my @why;
  push @why, "internal .syno host" if $line =~ /[a-z0-9-]\.syno\b/;
  push @why, "internal domain" if $line =~ /\bsynology\.inc\b|\bva[r]vis\.me\b/i;
  while ($line =~ /(?<![\d.])((?:10\.\d{1,3}|172\.(?:1[6-9]|2\d|3[01])|192\.168)\.\d{1,3}\.\d{1,3})(?!\.?\d)/g) {
    my $ip = $1;
    next if $ip =~ /^(?:192\.168\.[012]\.\d+|10\.0\.0\.\d+|10\.1\.2\.3|10\.2\.3\.4|10\.0\.2\.2|172\.17\.0\.1)$/;
    push @why, "private IPv4 address"; last;
  }
  push @why, "thread id" if $line =~ /\bt-\d{4}\b/;
  push @why, "home folder" if $line =~ m{/Users/[A-Za-z0-9._-]+};
  push @why, "secret" if $line =~ /BEGIN (?:RSA |OPENSSH |EC )?PRIVATE KEY|\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})\b/;
  push @why, "listed machine name" if length $names && $line =~ /(?<![A-Za-z0-9])(?:$names)(?![A-Za-z0-9])/i;
  next unless @why;
  my $text = length($line) > 160 ? substr($line, 0, 160) . "..." : $line;
  print "$path:$n: ", join(", ", @why), ": $text\n"; $found++;
  END {
    if ($found) {
      print STDERR "check-internal-names: $found line(s) name internal infrastructure; use a neutral word or a placeholder\n";
      exit 1;
    }
  }'
[[ $mode != tree ]] || echo "check-internal-names: clean"
