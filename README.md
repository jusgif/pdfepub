# pdfepub

Rust CLI converting PDF pages to reflowable EPUB 3 with optional Tesseract OCR.

## Status
Early implementation, **not production-ready**. Digital text extraction uses Poppler's `pdftotext`; OCR renders pages with `pdftoppm` and runs Tesseract. EPUB 3 navigation and NCX are generated. Chapter detection is currently heading markup within page-based sections, not true chapter splitting. Cover extraction, figure embedding, deskew, adaptive thresholding, repeated header/footer detection, multi-column reconstruction, and password-protected rendering need further work.

## Dependencies (Ubuntu / Codespaces)

```bash
sudo apt-get update
sudo apt-get install -y poppler-utils tesseract-ocr tesseract-ocr-eng
cargo build --release
cargo run -- --help
```

## Usage

```bash
cargo run --release -- input.pdf output.epub --ocr auto
cargo run --release -- scan.pdf scan.epub --ocr force --dpi 350 --ocr-lang eng
cargo run --release -- digital.pdf digital.epub --ocr off
```

Install additional Tesseract language packs for multilingual OCR (e.g. `tesseract-ocr-deu`), then pass `--ocr-lang eng+deu`.

## Validation

```bash
cargo test
unzip -t output.epub
epubcheck output.epub
```

## Security
Do not commit private PDFs to a public repository. External executables are invoked without a shell; inputs are passed as arguments. Untrusted PDFs should still be processed in a sandbox.

## Notes
The pipeline processes one PDF page at a time, but collects XHTML chapters in memory before packaging. Large PDFs may require a disk-spooled writer.
