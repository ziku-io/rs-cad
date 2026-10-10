use rs_cad::{dwg, dxf, format, raster, render_dwfx_all, render_dwfx_page};
use std::env;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
enum Output {
    Svg,
    Png,
    Jpeg,
}

fn usage() -> ! {
    eprintln!(
        "usage: rs-cad <inspect|render> INPUT [--page N] [--output DIRECTORY] [--format svg|png|jpeg]"
    );
    std::process::exit(2)
}

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("rs-cad: {message}");
    std::process::exit(1)
}

fn output_dir(input: &Path, supplied: Option<PathBuf>) -> PathBuf {
    supplied.unwrap_or_else(|| PathBuf::from(input.file_stem().unwrap_or_default()))
}

fn extension(output: Output) -> &'static str {
    match output {
        Output::Svg => "svg",
        Output::Png => "png",
        Output::Jpeg => "jpg",
    }
}

fn raster_format(output: Output) -> Option<raster::Format> {
    match output {
        Output::Svg => None,
        Output::Png => Some(raster::Format::Png),
        Output::Jpeg => Some(raster::Format::Jpeg),
    }
}

fn encode(svg: String, output: Output) -> Result<Vec<u8>, String> {
    match raster_format(output) {
        None => Ok(svg.into_bytes()),
        Some(format) => raster::render(&svg, format).map_err(|error| error.to_string()),
    }
}

fn main() {
    let mut args = env::args_os().skip(1);
    let command = args.next().unwrap_or_else(|| usage());
    let input = PathBuf::from(args.next().unwrap_or_else(|| usage()));
    let mut page = None;
    let mut output_dir_arg = None;
    let mut output = Output::Png;
    while let Some(flag) = args.next() {
        match flag.to_string_lossy().as_ref() {
            "--page" => {
                page = Some(
                    args.next()
                        .unwrap_or_else(|| usage())
                        .to_string_lossy()
                        .parse::<usize>()
                        .ok()
                        .filter(|number| *number > 0)
                        .unwrap_or_else(|| usage())
                        - 1,
                )
            }
            "--output" => {
                output_dir_arg = Some(PathBuf::from(args.next().unwrap_or_else(|| usage())))
            }
            "--format" => {
                output = match args
                    .next()
                    .unwrap_or_else(|| usage())
                    .to_string_lossy()
                    .as_ref()
                {
                    "svg" => Output::Svg,
                    "png" => Output::Png,
                    "jpeg" | "jpg" => Output::Jpeg,
                    _ => usage(),
                }
            }
            _ => usage(),
        }
    }
    let bytes = fs::read(&input)
        .unwrap_or_else(|error| fail(format!("cannot read {}: {error}", input.display())));
    let detected = format::detect(&bytes).unwrap_or_else(|error| fail(error));
    match command.to_string_lossy().as_ref() {
        "inspect" => match detected {
            format::InputFormat::Dwfx => match page {
                Some(index) => {
                    let (info, _) = render_dwfx_page(Cursor::new(bytes), index)
                        .unwrap_or_else(|error| fail(error));
                    println!(
                        "format=dwfx page={} width={} height={} paths={} glyphs={} canvases={} labels={} warnings={}",
                        index + 1,
                        info.width,
                        info.height,
                        info.paths,
                        info.glyphs,
                        info.canvases,
                        info.text.len(),
                        info.warnings.len()
                    );
                }
                None => {
                    let pages =
                        render_dwfx_all(Cursor::new(bytes)).unwrap_or_else(|error| fail(error));
                    println!("format=dwfx pages={}", pages.len());
                }
            },
            format::InputFormat::Dxf => {
                if page.is_some() {
                    usage()
                };
                let info = dxf::inspect(&bytes).unwrap_or_else(|error| fail(error));
                println!(
                    "format=dxf lines={} circles={} warnings={}",
                    info.lines,
                    info.circles,
                    info.warnings.len()
                );
            }
            format::InputFormat::Dwg => {
                if page.is_some() {
                    usage()
                };
                let info = dwg::inspect(&bytes).unwrap_or_else(|error| fail(error));
                println!("format=dwg version={:?} marker={}", info.version, info.code);
            }
        },
        "render" => {
            let directory = output_dir(&input, output_dir_arg);
            if directory.exists() && !directory.is_dir() {
                fail("output must be a directory");
            }
            let pages: Vec<(usize, String)> = match detected {
                format::InputFormat::Dwfx => match page {
                    Some(index) => vec![(
                        index,
                        render_dwfx_page(Cursor::new(bytes), index)
                            .unwrap_or_else(|error| fail(error))
                            .1,
                    )],
                    None => render_dwfx_all(Cursor::new(bytes))
                        .unwrap_or_else(|error| fail(error))
                        .into_iter()
                        .enumerate()
                        .map(|(index, (_, svg))| (index, svg))
                        .collect(),
                },
                format::InputFormat::Dxf => {
                    if page.is_some() {
                        usage()
                    };
                    vec![(0, dxf::render(&bytes).unwrap_or_else(|error| fail(error)))]
                }
                format::InputFormat::Dwg => {
                    fail("DWG rendering is not implemented; inspect reports the detected version")
                }
            };
            let rendered: Vec<(PathBuf, Vec<u8>)> = pages
                .into_iter()
                .map(|(index, svg)| {
                    let path =
                        directory.join(format!("page-{:03}.{}", index + 1, extension(output)));
                    (
                        path,
                        encode(svg, output).unwrap_or_else(|error| fail(error)),
                    )
                })
                .collect();
            fs::create_dir_all(&directory).unwrap_or_else(|error| {
                fail(format!("cannot create {}: {error}", directory.display()))
            });
            for (path, bytes) in rendered {
                fs::write(&path, bytes).unwrap_or_else(|error| {
                    fail(format!("cannot write {}: {error}", path.display()))
                });
            }
        }
        _ => usage(),
    }
}
