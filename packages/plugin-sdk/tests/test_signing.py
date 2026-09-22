"""Cross-language golden tests for opencapx_sdk.signing.

WHY: the Python SDK must byte-for-byte replicate the same v2 digest/signature format as Rust
`core::signing`; the goldens committed to the repo (fixtures/signing) are the shared anchor for
both implementations: the Python side both verifies the golden archive (`Rust → Python`
direction) and repacks with the same seed to get a digest + Ed25519 signature byte-for-byte
identical to the golden (`Python → Rust` direction).
"""
import importlib.util
import json
import zipfile
from pathlib import Path

import pytest

from opencapx_sdk import signing

# F8 — cryptography is an optional dependency (the SDK imports it lazily only on sign/verify paths).
# When missing, signature-related cases skip gracefully (same as the Rust side's plugin_sig
# cross-language guard, see plugin_sig.rs); CI installs it explicitly (cryptography), see
# .github/workflows/ci.yml; pure digest/numeric-dialect cases still run.
requires_crypto = pytest.mark.skipif(
    importlib.util.find_spec("cryptography") is None,
    reason="optional dep missing: pip install cryptography",
)

# __file__ = packages/plugin-sdk/tests/test_signing.py → parents[3] = repo root
FIXTURES = Path(__file__).resolve().parents[3] / "fixtures" / "signing"
GOLDEN = FIXTURES / "golden"
TRUSTED = FIXTURES / "trusted-keys.json"
KEY_ID = "com.opencapx.test-signing"


def _read_manifest(archive):
    with zipfile.ZipFile(archive) as z:
        return json.loads(z.read("opencapx-plugin.json"))


def _repack(src, dst, mutate):
    """Copy src entry by entry to dst, with entry content transformed by mutate(name, bytes)."""
    with zipfile.ZipFile(src) as zin, zipfile.ZipFile(
        dst, "w", zipfile.ZIP_DEFLATED
    ) as zout:
        for item in zin.infolist():
            data = mutate(item.filename, zin.read(item.filename))
            zout.writestr(item, data)


def test_digest_matches_golden():
    assert signing.digest_v2_for_dir(FIXTURES / "plugin") == (
        GOLDEN / "digest_v2.hex"
    ).read_text().strip()


@requires_crypto
def test_verify_golden_archive():
    status, key_id = signing.verify(GOLDEN / "signed.ocplugin", TRUSTED)
    assert (status, key_id) == ("trusted", KEY_ID)


@requires_crypto
def test_pack_reproduces_golden_digest_and_signature(tmp_path):
    out = tmp_path / "py.ocplugin"
    _, digest = signing.pack_dir(
        FIXTURES / "plugin",
        out,
        seed_hex=(FIXTURES / "key.seed.hex").read_text().strip(),
        key_id=KEY_ID,
    )
    assert digest == (GOLDEN / "digest_v2.hex").read_text().strip()
    # Recompute the archive digest to confirm pack's on-disk content matches the directory digest.
    assert signing.digest_v2(out) == digest
    # Ed25519 is deterministic → the signature is byte-for-byte identical to the golden.
    assert _read_manifest(out)["signature"]["sig"] == (
        GOLDEN / "signature.hex"
    ).read_text().strip()
    # A package we produced ourselves must pass verify (SDK self-consistency).
    assert signing.verify(out, TRUSTED) == ("trusted", KEY_ID)


@requires_crypto
def test_tamper_matrix(tmp_path):
    # 1) Unsigned archive → unsigned, same convention as Rust.
    assert signing.verify(GOLDEN / "unsigned.ocplugin", TRUSTED) == ("unsigned", None)

    # 2) Tamper with the manifest (sneak-edit permissions after signing, keeping the old sha256/signature) → tampered.
    tampered_manifest = tmp_path / "tampered-manifest.ocplugin"

    def mutate_manifest(name, data):
        if name != "opencapx-plugin.json":
            return data
        m = json.loads(data)
        m["permissions"] = ["image.read", "shell.exec"]
        return json.dumps(m, ensure_ascii=False, separators=(",", ":")).encode()

    _repack(GOLDEN / "signed.ocplugin", tampered_manifest, mutate_manifest)
    assert signing.verify(tampered_manifest, TRUSTED)[0] == "tampered"

    # 3) Tamper with a content entry (manifest unchanged) → digest changes → tampered.
    tampered_entry = tmp_path / "tampered-entry.ocplugin"

    def mutate_entry(name, data):
        if name == "bin/run.sh":
            return b"X" + data[1:]
        return data

    _repack(GOLDEN / "signed.ocplugin", tampered_entry, mutate_entry)
    assert signing.verify(tampered_entry, TRUSTED)[0] == "tampered"

    # 4) Digest matches but the signature is corrupt (keyId is still in trusted-keys) → bad-signature.
    wrong_sig = tmp_path / "wrong-sig.ocplugin"

    def mutate_sig(name, data):
        if name != "opencapx-plugin.json":
            return data
        m = json.loads(data)
        sig = m["signature"]["sig"]
        # Flip one hex character: length stays the same, but verification must fail.
        m["signature"]["sig"] = ("0" if sig[0] != "0" else "1") + sig[1:]
        return json.dumps(m, ensure_ascii=False, separators=(",", ":")).encode()

    _repack(GOLDEN / "signed.ocplugin", wrong_sig, mutate_sig)
    assert signing.verify(wrong_sig, TRUSTED)[0] == "bad-signature"


SEED = (FIXTURES / "key.seed.hex").read_text().strip()


def _make_plugin_dir(tmp_path, manifest):
    plugin = tmp_path / "plugin"
    (plugin / "bin").mkdir(parents=True)
    (plugin / "opencapx-plugin.json").write_text(
        json.dumps(manifest, ensure_ascii=False, separators=(",", ":")),
        encoding="utf-8",
    )
    (plugin / "bin" / "run.sh").write_text("echo ok\n", encoding="utf-8")
    return plugin


def test_ensure_signable_numbers_accepts_bool_and_int():
    assert signing.ensure_signable_numbers({"a": 1, "b": [True, 0], "c": "s"}) is None


def test_pack_dir_rejects_float_manifest(tmp_path):
    plugin = _make_plugin_dir(tmp_path, {"id": "com.x", "ratio": 1.5})
    with pytest.raises(ValueError):
        signing.pack_dir(plugin, tmp_path / "out.ocplugin", SEED, KEY_ID)
    assert not (tmp_path / "out.ocplugin").exists()


def test_pack_dir_rejects_out_of_range_int_manifest(tmp_path):
    plugin = _make_plugin_dir(tmp_path, {"id": "com.x", "huge": 2**64})
    with pytest.raises(ValueError):
        signing.pack_dir(plugin, tmp_path / "out.ocplugin", SEED, KEY_ID)
    assert not (tmp_path / "out.ocplugin").exists()


@requires_crypto
def test_pack_dir_accepts_integer_manifest(tmp_path):
    plugin = _make_plugin_dir(tmp_path, {"id": "com.x", "count": 7, "flag": True})
    out, digest = signing.pack_dir(plugin, tmp_path / "ok.ocplugin", SEED, KEY_ID)
    assert out.exists() and digest


@requires_crypto
def test_pack_dir_removes_partial_output_on_failure(tmp_path, monkeypatch):
    plugin = _make_plugin_dir(tmp_path, {"id": "com.x"})
    out = tmp_path / "partial.ocplugin"

    def boom(self, *args, **kwargs):
        raise RuntimeError("disk full")

    monkeypatch.setattr(zipfile.ZipFile, "writestr", boom)
    with pytest.raises(RuntimeError):
        signing.pack_dir(plugin, out, SEED, KEY_ID)
    assert not out.exists()


def test_cli_help_subcommand_prints_workflow(capsys):
    assert signing.main(["help"]) == 0
    out = capsys.readouterr().out
    for token in ("keygen", "pack", "verify", "OCX_SIGNING_KEY", "--trusted-keys"):
        assert token in out


def test_cli_top_level_help_flags(capsys):
    assert signing.main(["--help"]) == 0
    assert signing.main(["-h"]) == 0
    assert "keygen" in capsys.readouterr().out


def test_cli_no_args_is_usage_error(capsys):
    assert signing.main([]) == 1
    assert "keygen" in capsys.readouterr().err
