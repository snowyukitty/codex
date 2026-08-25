#!/usr/bin/env python3

import hashlib
import shutil
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path


INSTALL_SCRIPT = Path(__file__).with_name("install.ps1")
FIXTURE_CONTENT = b"codex installer digest fixture\n"
POWERSHELL_COMMANDS = tuple(
    dict.fromkeys(
        command
        for command in (
            shutil.which("powershell.exe"),
            shutil.which("pwsh"),
        )
        if command is not None
    )
)

POWERSHELL_HARNESS = textwrap.dedent(
    r"""
    param(
        [string]$InstallerPath,
        [string]$FixturePath,
        [string]$ExpectedDigest
    )

    $ErrorActionPreference = "Stop"
    $tokens = $null
    $parseErrors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile(
        $InstallerPath,
        [ref]$tokens,
        [ref]$parseErrors
    )
    if ($parseErrors.Count -ne 0) {
        throw "Failed to parse install.ps1: $($parseErrors[0].Message)"
    }

    foreach ($functionName in @("New-Sha256Hasher", "Test-ArchiveDigest")) {
        $functionAst = $ast.Find(
            {
                param($node)
                $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
                    $node.Name -eq $functionName
            },
            $true
        )
        if ($null -eq $functionAst) {
            throw "Could not find $functionName in install.ps1."
        }
        . ([scriptblock]::Create($functionAst.Extent.Text))
    }

    function Get-FileHash {
        throw "Test-ArchiveDigest must not call Get-FileHash."
    }

    $cspType = "System.Security.Cryptography.SHA256CryptoServiceProvider" -as [Type]
    if ($null -ne $cspType) {
        $cspProbe = $null
        try {
            $cspProbe = $cspType::new()
        }
        catch {
            # A platform may expose the type without supporting its constructor.
        }

        if ($null -ne $cspProbe) {
            try {
                $selectedHasher = New-Sha256Hasher
                try {
                    if ($selectedHasher.GetType() -ne $cspProbe.GetType()) {
                        throw "New-Sha256Hasher did not prefer the available CSP implementation."
                    }
                }
                finally {
                    $selectedHasher.Dispose()
                }
            }
            finally {
                $cspProbe.Dispose()
            }
        }
    }

    Test-ArchiveDigest -ArchivePath $FixturePath -ExpectedDigest $ExpectedDigest

    $fixtureDirectory = Split-Path -Parent $FixturePath
    $fixtureName = Split-Path -Leaf $FixturePath
    $originalLocation = Get-Location
    $originalCurrentDirectory = [Environment]::CurrentDirectory
    try {
        Set-Location -LiteralPath $fixtureDirectory
        [Environment]::CurrentDirectory = [System.IO.Path]::GetTempPath()
        Test-ArchiveDigest -ArchivePath ".\$fixtureName" -ExpectedDigest $ExpectedDigest
    }
    finally {
        Set-Location -LiteralPath $originalLocation
        [Environment]::CurrentDirectory = $originalCurrentDirectory
    }

    $wrongDigest = "0" * 64
    $expectedError = "Downloaded Codex archive checksum did not match expected digest. Expected $wrongDigest but got $ExpectedDigest."
    try {
        Test-ArchiveDigest -ArchivePath $FixturePath -ExpectedDigest $wrongDigest
        throw "Test-ArchiveDigest accepted an incorrect digest."
    }
    catch {
        if ($_.Exception.Message -cne $expectedError) {
            throw "Unexpected checksum error: $($_.Exception.Message)"
        }
    }

    $exclusiveStream = [System.IO.File]::Open(
        $FixturePath,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::ReadWrite,
        [System.IO.FileShare]::None
    )
    $exclusiveStream.Dispose()

    Write-Output "ok"
    """
).strip()


class InstallPs1Test(unittest.TestCase):
    @unittest.skipUnless(POWERSHELL_COMMANDS, "PowerShell is not available")
    def test_archive_digest_is_portable_and_preserves_literal_paths(self) -> None:
        expected_digest = hashlib.sha256(FIXTURE_CONTENT).hexdigest()

        with tempfile.TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)
            fixture_path = temp_path / "we[i]rd.bin"
            fixture_path.write_bytes(FIXTURE_CONTENT)
            harness_path = temp_path / "test-install-ps1.ps1"
            harness_path.write_text(POWERSHELL_HARNESS, encoding="utf-8")

            for powershell in POWERSHELL_COMMANDS:
                with self.subTest(powershell=Path(powershell).name):
                    result = subprocess.run(
                        [
                            powershell,
                            "-NoProfile",
                            "-NonInteractive",
                            "-ExecutionPolicy",
                            "Bypass",
                            "-File",
                            str(harness_path),
                            "-InstallerPath",
                            str(INSTALL_SCRIPT),
                            "-FixturePath",
                            str(fixture_path),
                            "-ExpectedDigest",
                            expected_digest,
                        ],
                        capture_output=True,
                        check=False,
                        text=True,
                        timeout=30,
                    )

                    self.assertEqual(
                        result.returncode,
                        0,
                        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}",
                    )
                    self.assertEqual(result.stdout.splitlines(), ["ok"])


if __name__ == "__main__":
    unittest.main()
