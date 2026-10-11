use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::path::Path;

const BLOCK: usize = 4 * 1024 * 1024;

/// Computes the SHA-256 of a file, parallelising for large inputs.
pub fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let size = f.metadata()?.len();

    let mut hasher = Sha256::new();
    if size < (BLOCK as u64) * 4 {
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    } else {
        let nblocks = ((size as usize) + BLOCK - 1) / BLOCK;
        let digests: Vec<[u8; 32]> = (0..nblocks)
            .into_par_iter()
            .map(|i| {
                use std::io::{Read, Seek, SeekFrom};
                let off = i as u64 * BLOCK as u64;
                let want = BLOCK.min((size - off) as usize);
                let mut f = std::fs::File::open(path).expect("reopen");
                let mut buf = vec![0u8; want];
                f.seek(SeekFrom::Start(off)).expect("seek");
                f.read_exact(&mut buf).expect("read block");
                let mut h = Sha256::new();
                h.update(&buf);
                h.finalize().into()
            })
            .collect();
        for d in digests {
            hasher.update(d);
        }
    }

    Ok(format!("{:X}", hasher.finalize()))
}

/// Fails loudly when the downloaded artifact does not match the manifest.
pub fn verify(path: &Path, expected: &str) -> Result<()> {
    if expected.trim().is_empty() || expected.eq_ignore_ascii_case("0") {
        bail!("manifest does not provide a SHA-256 for {}", path.display());
    }
    let actual = sha256_file(path)?;
    if actual.eq_ignore_ascii_case(expected.trim()) {
        Ok(())
    } else {
        let _ = std::fs::remove_file(path);
        bail!(
            "SHA-256 mismatch for {}\n  expected {}\n  actual   {}\n  the downloaded file was deleted",
            path.display(),
            expected,
            actual
        );
    }
}