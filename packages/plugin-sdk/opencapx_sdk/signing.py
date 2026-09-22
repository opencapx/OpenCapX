"""M2 v2 canonical digest and signing — byte-for-byte identical to Rust `core::signing` / `core::plugin_sig`.

WHY: plugin authors are Python-first; a package signed by the Rust CLI must be re-computable/re-signable
by the Python SDK, and vice versa. Once the two implementations silently drift on frame format, key
ordering, canonicalization, or path conventions, the signature chain silently mismatches between the
two tools. This module therefore strictly replicates Rust's byte-level conventions, and is pinned both
ways by the goldens committed to the repo (fixtures/signing).

Public interface:
    canonical_manifest(manifest) -> str
    digest_v2_for_dir(plugin_dir) -> str
    digest_v2(archive) -> str
    pack_dir(plugin_dir, out, seed_hex, key_id) -> (Path, digest)
    verify(archive, trusted_keys_path=None) -> (label, key_id_or_None)
    keygen(out_path) -> public_key_hex

CLI: `python3 -m opencapx_sdk.signing {keygen|pack|verify}` (reused by pack-ocplugin.sh).
"""
from __future__ import annotations

import hashlib
import hmac as _hmac
import json
import os
import sys
import zipfile
from pathlib import Path

MANIFEST_NAME = "opencapx-plugin.json"
# Frame domain strings (frozen): message prefixes for the v2 digest, the v2 signature, and the v1 HMAC channel.
DIGEST_DOMAIN = b"opencapx-canon-v2\n"
SIGN_DOMAIN = b"opencapx-v2\n"
HMAC_DOMAIN = b"opencapx-v1\n"

__all__ = [
    "canonical_manifest",
    "digest_v2",
    "digest_v2_for_dir",
    "ensure_signable_numbers",
    "pack_dir",
    "verify",
    "keygen",
    "main",
]


# ===== canonical =====

def canonical_manifest(manifest: dict) -> str:
    """Normalize the manifest into the byte form participating in the digest (byte-for-byte identical to Rust).

    WHY: strip `sha256`/`signature` (they are products of signing; including them would be self-referential);
    sort keys lexicographically at every level; compact with no whitespace; non-ASCII kept as raw UTF-8 —
    fully isomorphic to serde_json's (BTreeMap + to_string) output.
    """
    stripped = {
        k: v for k, v in manifest.items() if k not in ("sha256", "signature")
    }
    return json.dumps(
        stripped, sort_keys=True, ensure_ascii=False, separators=(",", ":")
    )


# ===== numbers =====

_SAFE_INT_LOW = -(2**63)
_SAFE_INT_HIGH = 2**64 - 1


def ensure_signable_numbers(value, path="$"):
    """Numeric dialect for signed manifests: Python (json) and serde_json (ryu) serialize floats /
    out-of-range integers differently → this would make digests diverge across languages. Reject
    explicitly at pack time; the bounds are hardcoded here (same convention as Rust
    `core::signing::ensure_signable_numbers`). bool is a subclass of int, so allow it.
    """
    if isinstance(value, bool):
        return
    if isinstance(value, float):
        raise ValueError(
            "manifest number %r at %s is not a 64-bit integer "
            "(floats / out-of-range integers are unsupported in signed manifests)"
            % (value, path)
        )
    if isinstance(value, int):
        if value < _SAFE_INT_LOW or value > _SAFE_INT_HIGH:
            raise ValueError(
                "manifest number %r at %s is not a 64-bit integer "
                "(floats / out-of-range integers are unsupported in signed manifests)"
                % (value, path)
            )
        return
    if isinstance(value, dict):
        for k, v in value.items():
            ensure_signable_numbers(v, "%s.%s" % (path, k))
        return
    if isinstance(value, (list, tuple)):
        for i, v in enumerate(value):
            ensure_signable_numbers(v, "%s[%d]" % (path, i))


# ===== digest =====

def _digest_from_parts(canonical: str, entries) -> str:
    """Concatenate per the frozen frame spec and SHA-256 (entries ordered by name's UTF-8 bytes).

    Frame: `"opencapx-canon-v2\\n" ‖ decimal(len(canonical)) ‖ "\\n" ‖ canonical
        ‖ Σ name ‖ "\\n" ‖ decimal(size) ‖ "\\n" ‖ bytes`.
    """
    acc = bytearray()
    acc += DIGEST_DOMAIN
    canonical_bytes = canonical.encode("utf-8")
    acc += str(len(canonical_bytes)).encode("ascii")
    acc += b"\n"
    acc += canonical_bytes
    for name, size, data in sorted(entries, key=lambda e: e[0].encode("utf-8")):
        acc += name.encode("utf-8")
        acc += b"\n"
        acc += str(size).encode("ascii")
        acc += b"\n"
        acc += data
    return hashlib.sha256(bytes(acc)).hexdigest()


def _is_manifest(rel_name: str) -> bool:
    """Manifest exclusion convention: same name at root or in any subdirectory (matches Rust)."""
    return rel_name == MANIFEST_NAME or rel_name.endswith("/" + MANIFEST_NAME)


def _collect_dir_entries(root: Path):
    """Recursively collect regular files: relative POSIX paths, skip non-regular files like symlinks, exclude the manifest."""
    out = []

    def walk(cur: Path) -> None:
        with os.scandir(cur) as it:
            children = sorted(it, key=lambda e: e.name)
        for entry in children:
            if entry.is_dir(follow_symlinks=False):
                walk(Path(entry.path))
            elif entry.is_file(follow_symlinks=False):
                rel = os.path.relpath(entry.path, root).replace(os.sep, "/")
                if _is_manifest(rel):
                    continue
                data = Path(entry.path).read_bytes()
                out.append((rel, len(data), data))

    walk(root)
    out.sort(key=lambda e: e[0].encode("utf-8"))
    return out


def _collect_zip_entries(zf: zipfile.ZipFile):
    """Collect content entries from an opened zip: skip directories, skip the manifest, ordered by name bytes."""
    out = []
    for info in zf.infolist():
        if info.is_dir():
            continue
        name = info.filename
        if _is_manifest(name):
            continue
        data = zf.read(info)
        out.append((name, len(data), data))
    out.sort(key=lambda e: e[0].encode("utf-8"))
    return out


def digest_v2_for_dir(plugin_dir) -> str:
    """Compute the v2 digest for an unpacked plugin directory (used by pack)."""
    plugin_dir = Path(plugin_dir)
    manifest_text = (plugin_dir / MANIFEST_NAME).read_text(encoding="utf-8")
    canonical = canonical_manifest(json.loads(manifest_text))
    return _digest_from_parts(canonical, _collect_dir_entries(plugin_dir))


def digest_v2(archive) -> str:
    """Compute the v2 digest for a packed `.ocplugin` (the manifest section comes from inside the archive)."""
    with zipfile.ZipFile(archive) as zf:
        manifest_text = zf.read(MANIFEST_NAME).decode("utf-8")
        entries = _collect_zip_entries(zf)
    canonical = canonical_manifest(json.loads(manifest_text))
    return _digest_from_parts(canonical, entries)


# ===== keys =====

def _resolve_seed(seed_hex) -> bytes:
    """Parse a 32-byte seed: 64 hex characters, `@path` (file content after trim), or a raw 32-byte value."""
    if isinstance(seed_hex, (bytes, bytearray)):
        raw = bytes(seed_hex)
        if len(raw) != 32:
            raise ValueError("seed must be 32 bytes, got %d" % len(raw))
        return raw
    text = seed_hex
    if isinstance(text, str) and text.startswith("@"):
        text = Path(text[1:]).read_text(encoding="utf-8")
    text = str(text).strip()
    if len(text) != 64:
        raise ValueError("seed must be 64 hex characters, got %d" % len(text))
    return bytes.fromhex(text)


def keygen(out_path) -> str:
    """Generate a random Ed25519 identity: write the seed's 64-hex (with a trailing newline), return the public key hex.

    WHY: the seed is the identity root; silently overwriting it would permanently break the author's
    link between the old private key and already-published signed packages. Prefer an error over a
    destructive overwrite (same policy as the Rust CLI).
    """
    out = Path(out_path)
    if out.exists():
        raise FileExistsError("%s already exists, refusing to overwrite (use a different --out or delete it first)" % out)
    from cryptography.hazmat.primitives import serialization
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

    seed = os.urandom(32)
    sk = Ed25519PrivateKey.from_private_bytes(seed)
    pk_hex = sk.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    ).hex()
    out.write_text(seed.hex() + "\n", encoding="utf-8")
    return pk_hex


def _ed25519_sign(seed: bytes, message: bytes) -> str:
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

    sk = Ed25519PrivateKey.from_private_bytes(seed)
    return sk.sign(message).hex()


# ===== pack =====

def pack_dir(plugin_dir, out, seed_hex, key_id):
    """Pack a plugin directory into a signed `.ocplugin`, returning `(out_path, digest)`.

    The manifest section is written first, then content files in relative-path byte order — mirroring Rust pack's layout.
    """
    plugin_dir = Path(plugin_dir)
    out = Path(out)
    manifest = json.loads(
        (plugin_dir / MANIFEST_NAME).read_text(encoding="utf-8")
    )
    ensure_signable_numbers(manifest)
    digest = digest_v2_for_dir(plugin_dir)
    seed = _resolve_seed(seed_hex)
    sig_hex = _ed25519_sign(seed, SIGN_DOMAIN + digest.encode("utf-8"))

    signed = dict(manifest)
    signed["sha256"] = digest
    signed["signature"] = {"alg": "ed25519", "keyId": key_id, "sig": sig_hex}
    # The on-disk manifest uses the same compact form as canonical (stable key order, non-ASCII kept raw).
    signed_text = json.dumps(
        signed, sort_keys=True, ensure_ascii=False, separators=(",", ":")
    )

    entries = _collect_dir_entries(plugin_dir)
    zf = None
    try:
        zf = zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED)
        zf.writestr(MANIFEST_NAME, signed_text.encode("utf-8"))
        for name, _size, data in entries:
            zf.writestr(name, data)
    except BaseException:
        # WHY: a half-written archive would masquerade as a valid .ocplugin and mislead downstream
        # verification; on error close and delete, mirroring Rust pack's delete-on-failure.
        if zf is not None:
            zf.close()
        try:
            os.remove(out)
        except OSError:
            pass
        raise
    zf.close()
    return out, digest


# ===== verify =====

def _default_trusted_keys_path() -> str:
    env = os.environ.get("OPENCAPX_TRUSTED_KEYS")
    if env:
        return env
    return os.path.join(os.path.expanduser("~"), ".opencapx", "trusted-keys.json")


def _load_trusted_keys(trusted_keys_path=None):
    """Read trusted-keys, returning {key_id: ("hmac"|"ed25519", bytes)}; skip invalid entries.

    Two shapes: a string value = legacy HMAC secret (hex-decoded); an object
    `{alg:"ed25519", publicKey:"<64 hex>"}` = Ed25519 public key. Same convention as Rust.
    """
    if trusted_keys_path is None:
        trusted_keys_path = _default_trusted_keys_path()
    try:
        text = Path(trusted_keys_path).read_text(encoding="utf-8")
    except OSError:
        return {}
    try:
        parsed = json.loads(text)
    except ValueError:
        return {}
    if not isinstance(parsed, dict):
        return {}
    out = {}
    for key_id, value in parsed.items():
        if isinstance(value, str):
            try:
                out[key_id] = ("hmac", bytes.fromhex(value.strip()))
            except ValueError:
                pass
            continue
        if not isinstance(value, dict) or value.get("alg") != "ed25519":
            continue
        pk = value.get("publicKey")
        if not isinstance(pk, str):
            continue
        try:
            raw = bytes.fromhex(pk.strip())
        except ValueError:
            continue
        if len(raw) == 32:
            out[key_id] = ("ed25519", raw)
    return out


def _verify_hmac(archive, declared_sha, key_id, sig_hex, trusted_keys_path):
    """v1 local channel: the manifest is not part of the digest; signature = HMAC(secret, "opencapx-v1\\n"+hash)."""
    try:
        declared_bytes = bytes.fromhex(declared_sha.strip())
    except ValueError:
        return ("malformed-signature", None)
    try:
        actual = _compute_archive_hash(archive)
    except Exception:
        return ("malformed-signature", None)
    if declared_bytes.hex().lower() != actual.lower():
        return ("tampered", None)
    entry = _load_trusted_keys(trusted_keys_path).get(key_id)
    if entry is None or entry[0] != "hmac":
        return ("unknown-key", key_id)
    expected = _hmac.new(
        entry[1], HMAC_DOMAIN + actual.encode("utf-8"), hashlib.sha256
    ).hexdigest()
    if expected.lower() != sig_hex.lower():
        return ("bad-signature", key_id)
    return ("trusted", key_id)


def _verify_ed25519(archive, declared_sha, key_id, sig_hex, trusted_keys_path):
    """v2 distribution channel: the digest covers the manifest; signature = Ed25519("opencapx-v2\\n"+digest_hex)."""
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

    try:
        digest = digest_v2(archive)
    except Exception:
        return ("malformed-signature", None)
    if declared_sha.lower() != digest.lower():
        return ("tampered", None)
    entry = _load_trusted_keys(trusted_keys_path).get(key_id)
    if entry is None or entry[0] != "ed25519":
        return ("unknown-key", key_id)
    try:
        pub = Ed25519PublicKey.from_public_bytes(entry[1])
    except Exception:
        return ("malformed-signature", None)
    try:
        sig_bytes = bytes.fromhex(sig_hex.strip())
    except ValueError:
        return ("malformed-signature", None)
    if len(sig_bytes) != 64:
        return ("malformed-signature", None)
    try:
        pub.verify(sig_bytes, SIGN_DOMAIN + digest.encode("utf-8"))
    except Exception:
        return ("bad-signature", key_id)
    return ("trusted", key_id)


def _compute_archive_hash(archive) -> str:
    """v1 archive hash: concatenate non-manifest entries in name byte order as `<name>\\n<size>\\n<bytes>`."""
    with zipfile.ZipFile(archive) as zf:
        entries = _collect_zip_entries(zf)
    acc = bytearray()
    for name, size, data in entries:
        acc += name.encode("utf-8")
        acc += b"\n"
        acc += str(size).encode("ascii")
        acc += b"\n"
        acc += data
    return hashlib.sha256(bytes(acc)).hexdigest()


def verify(archive, trusted_keys_path=None):
    """Verify a `.ocplugin`, returning `(label, key_id_or_None)` (same table as Rust VerifyOutcome.label)."""
    try:
        with zipfile.ZipFile(archive) as zf:
            manifest_text = zf.read(MANIFEST_NAME).decode("utf-8")
    except Exception:
        return ("unsigned", None)
    try:
        manifest = json.loads(manifest_text)
    except ValueError:
        return ("unsigned", None)
    if not isinstance(manifest, dict):
        return ("unsigned", None)
    declared_sha = manifest.get("sha256")
    sig = manifest.get("signature")
    # Missing sha256 or missing signature → unsigned (same convention as Rust parse_sig_fields).
    if not isinstance(declared_sha, str) or not isinstance(sig, dict):
        return ("unsigned", None)
    key_id = sig.get("keyId")
    sig_hex = sig.get("sig")
    if not isinstance(key_id, str) or not isinstance(sig_hex, str):
        return ("unsigned", None)
    alg = sig.get("alg")
    if alg is None or alg == "hmac-v1":
        return _verify_hmac(archive, declared_sha, key_id, sig_hex, trusted_keys_path)
    if alg == "ed25519":
        return _verify_ed25519(
            archive, declared_sha, key_id, sig_hex, trusted_keys_path
        )
    return ("malformed-signature", None)


# ===== CLI (mirrors Rust keygen/pack/verify, called by pack-ocplugin.sh) =====

def _usage() -> None:
    print(
        "OpenCapX signing CLI (Python SDK)\n"
        "  python3 -m opencapx_sdk.signing keygen [--out <path.hex>]\n"
        "  python3 -m opencapx_sdk.signing pack <plugin-dir> "
        "--key <seed-hex|@file> --key-id <id> [--out <file.ocplugin>]\n"
        "  python3 -m opencapx_sdk.signing verify <file> [--trusted-keys <path>]\n"
        "  python3 -m opencapx_sdk.signing help",
        file=sys.stderr,
    )


def _help() -> None:
    """Author-facing full help (help subcommand / -h): usage + typical workflow + key discipline."""
    print(
        "OpenCapX plugin signing tool (Python SDK) — byte-for-byte same format as the Rust CLI\n"
        "\n"
        "Usage:\n"
        "  python3 -m opencapx_sdk.signing <command> [options]\n"
        "\n"
        "Commands:\n"
        "  keygen [--out <path.hex>]\n"
        "      Generate an Ed25519 key pair: write the seed to a file (default opencapx-signing.key.hex),\n"
        "      print the public key to stdout. Requires the optional cryptography dependency.\n"
        "  pack <plugin-dir> --key <seed-hex|@file> --key-id <id> [--out <file.ocplugin>]\n"
        "      Pack a plugin directory and sign a v2-signed .ocplugin.\n"
        "  verify <file> [--trusted-keys <path>]\n"
        "      Verify a .ocplugin. Exit codes: 0=trusted; 2=unsigned/unknown-key; 1=corrupt/IO.\n"
        "  help\n"
        "      Show this help.\n"
        "\n"
        "Typical workflow:\n"
        "  1. keygen → seed goes only into CI Secrets (OCX_SIGNING_KEY); public key + keyId committed to the registry\n"
        "  2. pack   → produces .ocplugin (tag flow in the plugin template .github/workflows/release.yml)\n"
        "  3. verify → automated-gate dry run (manifest/signature chain/static scan/declaration reconciliation/dependencies)\n"
        "\n"
        "Key discipline:\n"
        "  * the seed never enters the repo (.gitignore includes *.key.hex); losing it = cannot publish updates under the same key\n"
        "  * keyId is the publisher identity; changing it triggers a \"publisher changed\" re-confirmation on the user side\n"
        "  * Reference: docs/plugin-signing.md (author guide) · docs/plugin-review.md (review handbook)"
    )


def _opt(args, name):
    """Fetch an optional `--flag value` argument (same semantics as Rust signing_opt)."""
    it = iter(args)
    for a in it:
        if a == name:
            return next(it, None)
    return None


def _has_help(args) -> bool:
    return any(a in ("-h", "--help") for a in args)


def _cli_keygen(args) -> int:
    if _has_help(args):
        _usage()
        return 0
    out = _opt(args, "--out") or "opencapx-signing.key.hex"
    try:
        pk_hex = keygen(out)
    except FileExistsError as e:
        print("keygen: %s" % e, file=sys.stderr)
        return 1
    except OSError as e:
        print("keygen: failed to write %s: %s" % (out, e), file=sys.stderr)
        return 1
    except ImportError:
        # WHY: cryptography is an optional dependency; when missing, give a one-line actionable fix instead of a traceback.
        print("keygen failed: pip install cryptography", file=sys.stderr)
        return 1
    print(json.dumps({"out": out, "publicKey": pk_hex}, sort_keys=True))
    print(
        'suggested trusted-keys entry: {"<keyId>":{"alg":"ed25519","publicKey":"%s"}}' % pk_hex
    )
    return 0


def _cli_pack(args) -> int:
    if _has_help(args):
        _usage()
        return 0
    if not args or args[0].startswith("--"):
        _usage()
        return 1
    dir_arg = args[0]
    key_arg = _opt(args, "--key")
    if key_arg is None:
        print("pack: missing --key <seed-hex|@file>", file=sys.stderr)
        return 1
    key_id = _opt(args, "--key-id")
    if key_id is None:
        print("pack: missing --key-id <id>", file=sys.stderr)
        return 1
    try:
        manifest = json.loads(
            (Path(dir_arg) / MANIFEST_NAME).read_text(encoding="utf-8")
        )
    except OSError as e:
        print("pack: failed to read %s: %s" % (dir_arg, e), file=sys.stderr)
        return 1
    except ValueError as e:
        print("pack: manifest is invalid JSON: %s" % e, file=sys.stderr)
        return 1
    if not isinstance(manifest, dict) or not isinstance(manifest.get("id"), str):
        print("pack: manifest missing id", file=sys.stderr)
        return 1
    version = manifest.get("version")
    version = version if isinstance(version, str) else "0.0.0"
    out = _opt(args, "--out") or "%s-%s.ocplugin" % (manifest["id"], version)
    try:
        out_path, digest = pack_dir(dir_arg, out, key_arg, key_id)
    except Exception as e:
        print("pack: %s" % e, file=sys.stderr)
        return 1
    print(json.dumps({"out": str(out_path), "digest": digest, "keyId": key_id}, sort_keys=True))
    return 0


def _cli_verify(args) -> int:
    if _has_help(args):
        _usage()
        return 0
    if not args or args[0].startswith("--"):
        _usage()
        return 1
    file = args[0]
    trusted_keys = _opt(args, "--trusted-keys")
    path = Path(file)
    # WHY: verify conservatively returns unsigned for files it cannot open, but the CLI must distinguish
    # "IO/format error (1)" from "valid but unsigned (2)", consistent with the Rust CLI's exit-code convention.
    if not path.is_file():
        print("verify: file does not exist or is not a regular file: %s" % file, file=sys.stderr)
        return 1
    if not zipfile.is_zipfile(path):
        print("verify: not a valid .ocplugin (zip) or cannot open: %s" % file, file=sys.stderr)
        return 1
    status, key_id = verify(path, trusted_keys)
    obj = {"status": status}
    if key_id is not None:
        obj["keyId"] = key_id
    print(json.dumps(obj, sort_keys=True))
    print("verify: %s -> %s" % (file, status), file=sys.stderr)
    if status == "trusted":
        return 0
    if status in ("unsigned", "unknown-key"):
        return 2
    return 1


def main(argv=None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if not args:
        _usage()
        return 1
    cmd, rest = args[0], args[1:]
    if cmd in ("-h", "--help", "help"):
        _help()
        return 0
    if cmd == "keygen":
        return _cli_keygen(rest)
    if cmd == "pack":
        return _cli_pack(rest)
    if cmd == "verify":
        return _cli_verify(rest)
    _usage()
    return 1


if __name__ == "__main__":
    sys.exit(main())
