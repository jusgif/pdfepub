use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};
use regex::Regex;
use std::{collections::HashMap, fs, io::{Read, Write}, path::{Path, PathBuf}, process::Command};
use tempfile::tempdir;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq)]
enum OcrMode { Auto, Force, Off }

#[derive(Parser, Debug)]
#[command(name="pdfepub", about="Convert PDFs into reflowable EPUB 3 with optional OCR")]
struct Args {
    input: PathBuf,
    output: Option<PathBuf>,
    #[arg(long, value_enum, default_value="auto")]
    ocr: OcrMode,
    #[arg(long, default_value="eng")]
    ocr_lang: String,
    #[arg(long, default_value_t=300, value_parser=clap::value_parser!(u32).range(300..=600))]
    dpi: u32,
    #[arg(long)]
    title: Option<String>,
    #[arg(long)]
    author: Option<String>,
    #[arg(long, default_value="en")]
    lang: String,
    #[arg(long)]
    no_cover: bool,
    #[arg(long, default_value=r"(?i)^(chapter|part|section)\s+[0-9ivxlcm]+")]
    chapter_regex: String,
    #[arg(long)]
    password: Option<String>,
}
fn run(program: &str, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new(program).args(args).output()
        .with_context(|| format!("Missing required executable: {program}"))?;
    if !out.status.success() {
        bail!("{program} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}
fn escape(s: &str) -> String {
    s.chars().filter(|c| !c.is_control() || *c=='\n' || *c=='\t')
        .fold(String::new(), |mut a,c| { match c {
            '&'=>a.push_str("&amp;"), '<'=>a.push_str("&lt;"),
            '>'=>a.push_str("&gt;"), '"'=>a.push_str("&quot;"),
            '\''=>a.push_str("&apos;"), _=>a.push(c)
        }; a })
}
fn clean_text(s: &str) -> String {
    let hyphen = Regex::new(r"(?m)([[:alpha:]])-\n([[:alpha:]])").unwrap();
    let joined = hyphen.replace_all(s, "$1$2");
    joined.replace("\r", "")
}
fn paragraphs(s: &str, heading: &Regex) -> String {
    let mut result = String::new();
    let mut paragraph = Vec::<String>::new();
    let flush = |p: &mut Vec<String>, out: &mut String| {
        if !p.is_empty() {
            out.push_str("<p>");
            out.push_str(&escape(&p.join(" ")));
            out.push_str("</p>\n");
            p.clear();
        }
    };
    for raw in s.lines() {
        let line = raw.trim();
        if line.is_empty() { flush(&mut paragraph, &mut result); continue; }
        if heading.is_match(line) {
            flush(&mut paragraph, &mut result);
            result.push_str(&format!("<h2>{}</h2>\n", escape(line)));
        } else if line.starts_with("• ") || line.starts_with("- ") {
            flush(&mut paragraph, &mut result);
            result.push_str(&format!("<p class=\"list\">{}</p>\n", escape(line)));
        } else { paragraph.push(line.to_owned()); }
    }
    flush(&mut paragraph, &mut result);
    result
}
fn write_entry(zip: &mut ZipWriter<fs::File>, name: &str, content: &[u8], stored: bool) -> Result<()> {
    let method = if stored { CompressionMethod::Stored } else { CompressionMethod::Deflated };
    zip.start_file(name, SimpleFileOptions::default().compression_method(method))?;
    zip.write_all(content)?;
    Ok(())
}
fn pdfinfo(input: &Path, password: Option<&str>) -> Result<HashMap<String,String>> {
    let mut args = Vec::new();
    if let Some(p) = password { args.extend(["-upw", p]); }
    let path = input.to_str().context("PDF path is not valid UTF-8")?;
    args.push(path);
    let data = run("pdfinfo", &args)?;
    Ok(String::from_utf8_lossy(&data).lines().filter_map(|l| {
        l.split_once(':').map(|(k,v)|(k.trim().to_string(),v.trim().to_string()))
    }).collect())
}
fn main() -> Result<()> {
    let a = Args::parse();
    if !a.input.is_file() { bail!("Input PDF does not exist: {}", a.input.display()); }
    let info = pdfinfo(&a.input, a.password.as_deref())
        .context("Cannot inspect PDF (encrypted PDFs may need --password)")?;
    let pages: usize = info.get("Pages").context("pdfinfo did not report page count")?.parse()?;
    if pages == 0 { bail!("PDF contains no pages"); }
    let title = a.title.clone().or_else(|| info.get("Title").filter(|s| !s.is_empty()).cloned())
        .unwrap_or_else(|| a.input.file_stem().unwrap_or_default().to_string_lossy().to_string());
    let author = a.author.clone().or_else(|| info.get("Author").cloned()).unwrap_or_default();
    let output = a.output.clone().unwrap_or_else(|| a.input.with_extension("epub"));
    let heading = Regex::new(&a.chapter_regex).context("Invalid chapter regex")?;
    let work = tempdir()?;
    let pdf = a.input.to_str().context("Invalid PDF path")?;
    if a.ocr != OcrMode::Off {
        let langs = String::from_utf8_lossy(&run("tesseract", &["--list-langs"])?).to_string();
        for l in a.ocr_lang.split('+') {
            if !langs.lines().any(|x| x.trim()==l) { bail!("Tesseract language pack missing: {l}"); }
        }
    }
    let mut chapters = Vec::<(String,String)>::new();
    for n in 1..=pages {
        eprintln!("Processing page {n}/{pages}");
        let page = n.to_string();
        let mut txt_args = Vec::new();
        if let Some(p) = a.password.as_deref() { txt_args.extend(["-upw",p]); }
        txt_args.extend(["-f",&page,"-l",&page,"-layout",pdf,"-"]);
        let extracted = run("pdftotext",&txt_args)?;
        let digital = String::from_utf8_lossy(&extracted).to_string();
        let bad = digital.chars().filter(|c| *c=='\u{fffd}' || *c=='\0').count();
        let good = digital.chars().filter(|c| c.is_alphanumeric()).count();
        let use_ocr = a.ocr==OcrMode::Force || (a.ocr==OcrMode::Auto && (good<30 || bad>good/10));
        let text = if use_ocr {
            let prefix = work.path().join(format!("page-{n}"));
            let pref = prefix.to_str().context("Invalid temporary path")?;
            let dpi = a.dpi.to_string();
            let mut render_args = Vec::new();
            if let Some(p) = a.password.as_deref() { render_args.extend(["-upw",p]); }
            render_args.extend(["-f",&page,"-l",&page,"-r",&dpi,"-gray","-singlefile","-png",pdf,pref]);
            run("pdftoppm",&render_args)?;
            let png = prefix.with_extension("png");
            let pngpath = png.to_str().context("Invalid temporary image path")?;
            let recognized = run("tesseract",&[pngpath,"stdout","-l",&a.ocr_lang,"--psm","3"])?;
            let _ = fs::remove_file(png);
            String::from_utf8_lossy(&recognized).to_string()
        } else { digital };
        let body = paragraphs(&clean_text(&text),&heading);
        chapters.push((format!("Page {n}"),body));
    }
    let file = fs::File::create(&output).with_context(||format!("Cannot create {}",output.display()))?;
    let mut zip = ZipWriter::new(file);
    write_entry(&mut zip,"mimetype",b"application/epub+zip",true)?;
    write_entry(&mut zip,"META-INF/container.xml",br#"<?xml version="1.0" encoding="UTF-8"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,false)?;
    let css = "body{font-family:serif;line-height:1.5;margin:5%;color:inherit;background:transparent}p{margin:0 0 1em}h1,h2{font-family:sans-serif;break-after:avoid}img{max-width:100%;height:auto}.list{margin-left:1.5em}";
    write_entry(&mut zip,"OEBPS/style.css",css.as_bytes(),false)?;
    let mut manifest = String::from("<item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/><item id=\"ncx\" href=\"toc.ncx\" media-type=\"application/x-dtbncx+xml\"/><item id=\"css\" href=\"style.css\" media-type=\"text/css\"/>");
    let mut spine = String::new();
    let mut nav = String::new();
    let mut ncx = String::new();
    for (i,(label,body)) in chapters.iter().enumerate() {
        let num=i+1;
        let name=format!("chapter-{num}.xhtml");
        let xhtml=format!(r#"<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml" xml:lang="{lang}"><head><title>{label}</title><link rel="stylesheet" type="text/css" href="style.css"/></head><body><section><h1>{label}</h1>{body}</section></body></html>"#,lang=escape(&a.lang),label=escape(label));
        write_entry(&mut zip,&format!("OEBPS/{name}"),xhtml.as_bytes(),false)?;
        manifest.push_str(&format!("<item id=\"c{num}\" href=\"{name}\" media-type=\"application/xhtml+xml\"/>"));
        spine.push_str(&format!("<itemref idref=\"c{num}\"/>"));
        nav.push_str(&format!("<li><a href=\"{name}\">{}</a></li>",escape(label)));
        ncx.push_str(&format!("<navPoint id=\"n{num}\" playOrder=\"{num}\"><navLabel><text>{}</text></navLabel><content src=\"{name}\"/></navPoint>",escape(label)));
    }
    let navdoc=format!(r#"<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Contents</title></head><body><nav epub:type="toc" id="toc"><h1>Contents</h1><ol>{nav}</ol></nav></body></html>"#);
    write_entry(&mut zip,"OEBPS/nav.xhtml",navdoc.as_bytes(),false)?;
    let id=uuid::Uuid::new_v4().to_string();
    let ncxdoc=format!(r#"<?xml version="1.0" encoding="utf-8"?><ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1"><head><meta name="dtb:uid" content="{id}"/></head><docTitle><text>{}</text></docTitle><navMap>{ncx}</navMap></ncx>"#,escape(&title));
    write_entry(&mut zip,"OEBPS/toc.ncx",ncxdoc.as_bytes(),false)?;
    let opf=format!(r#"<?xml version="1.0" encoding="utf-8"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bookid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/"><dc:identifier id="bookid">urn:uuid:{id}</dc:identifier><dc:title>{}</dc:title><dc:creator>{}</dc:creator><dc:language>{}</dc:language><meta property="dcterms:modified">2026-01-01T00:00:00Z</meta></metadata><manifest>{manifest}</manifest><spine toc="ncx">{spine}</spine></package>"#,escape(&title),escape(&author),escape(&a.lang));
    write_entry(&mut zip,"OEBPS/content.opf",opf.as_bytes(),false)?;
    zip.finish()?;
    eprintln!("Created {}",output.display());
    if !a.no_cover { eprintln!("Note: cover extraction is not yet implemented"); }
    Ok(())
}
