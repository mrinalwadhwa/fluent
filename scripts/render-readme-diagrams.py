#!/usr/bin/env python3

"""Render the README diagrams from their tracked SVG sources."""

from __future__ import annotations

import argparse
import math
import shutil
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SOURCE_DIR = ROOT / "documentation" / "readme-diagrams" / "sources"
OUTPUT_DIR = ROOT / ".github" / "assets"
THEMES = ("light", "dark")
REQUIRED_FONT = "Menlo"
REQUIRED_TOOLS = ("rsvg-convert", "ffmpeg", "gifsicle", "fc-match")


@dataclass(frozen=True)
class Diagram:
    name: str
    width: int
    height: int
    animate: bool = False


@dataclass(frozen=True)
class Track:
    points: tuple[tuple[float, float], ...]
    cycles: int = 1
    offset: float = 0.0
    particles: int = 1


DIAGRAMS = (
    Diagram("fluent-at-a-glance", 2280, 1686, animate=True),
    Diagram("fluent-overall-flow", 2040, 1170),
    Diagram("how-you-tell-fluent", 2040, 1260),
    Diagram("how-fluent-builds", 2040, 1050),
)

FLOW_TRACKS = (
    # Observation sources converge on Observations.
    Track(((46, 104), (46, 126), (142, 126), (142, 135)), cycles=1, offset=0.00),
    Track(((113, 104), (113, 126), (142, 126), (142, 135)), cycles=2, offset=0.25),
    Track(((182.5, 104), (182.5, 126), (142, 126), (142, 135)), cycles=1, offset=0.50),
    Track(((263, 104), (263, 126), (142, 126), (142, 135)), cycles=1, offset=0.75),
    # Primary production flow.
    Track(((242, 221), (277, 221)), cycles=2, offset=0.05),
    Track(((480, 221), (515, 221)), cycles=2, offset=0.38),
    Track(
        ((718, 221), (738, 221), (738, 427), (721, 427)),
        cycles=1,
        offset=0.12,
        particles=2,
    ),
    Track(((518, 427), (483, 427)), cycles=2, offset=0.65),
    Track(((280, 427), (245, 427)), cycles=2, offset=0.88),
    Track(
        ((42, 427), (22, 427), (22, 126), (43, 126)),
        cycles=1,
        offset=0.18,
        particles=2,
    ),
    # Revision and learning loops.
    Track(((690, 181), (690, 198), (548, 198), (548, 181)), cycles=1, offset=0.40),
    Track(((618, 340), (618, 307)), cycles=2, offset=0.30),
    Track(
        ((548, 340), (548, 322), (142, 322), (142, 307)),
        cycles=1,
        offset=0.05,
        particles=2,
    ),
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Render tracked README diagram assets from their SVG sources."
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="render into a temporary directory and fail if tracked outputs differ",
    )
    return parser.parse_args()


def validate_inputs() -> None:
    missing_sources = [
        SOURCE_DIR / f"{diagram.name}-{theme}.svg"
        for diagram in DIAGRAMS
        for theme in THEMES
        if not (SOURCE_DIR / f"{diagram.name}-{theme}.svg").is_file()
    ]
    if missing_sources:
        formatted = "\n".join(
            f"  - {path.relative_to(ROOT)}" for path in missing_sources
        )
        raise SystemExit(f"missing README diagram sources:\n{formatted}")

    missing_tools = [tool for tool in REQUIRED_TOOLS if shutil.which(tool) is None]
    if missing_tools:
        formatted = ", ".join(missing_tools)
        raise SystemExit(
            "missing tools required to render README diagrams: "
            f"{formatted}\nInstall librsvg, ffmpeg, gifsicle, and fontconfig, "
            "then try again."
        )

    font_match = subprocess.run(
        ["fc-match", "--format=%{family}", REQUIRED_FONT],
        check=False,
        capture_output=True,
        text=True,
    )
    resolved_families = {
        family.strip() for family in font_match.stdout.split(",") if family.strip()
    }
    if font_match.returncode != 0 or REQUIRED_FONT not in resolved_families:
        raise SystemExit(
            f"missing font required to render README diagrams: {REQUIRED_FONT}\n"
            "Use the documented rendering environment rather than accepting "
            "a fallback font."
        )

    validate_tracks(FLOW_TRACKS)


def validate_tracks(tracks: tuple[Track, ...]) -> None:
    invalid = [
        index
        for index, track in enumerate(tracks, start=1)
        if isinstance(track.cycles, bool)
        or not isinstance(track.cycles, int)
        or track.cycles < 1
    ]
    if invalid:
        formatted = ", ".join(str(index) for index in invalid)
        raise SystemExit(
            "every animation track must complete a positive whole number of "
            f"cycles for a seamless loop; invalid track(s): {formatted}"
        )


def run(command: list[str]) -> None:
    try:
        subprocess.run(command, check=True, stdin=subprocess.DEVNULL)
    except subprocess.CalledProcessError as error:
        rendered_command = " ".join(command)
        raise RuntimeError(
            f"diagram rendering command failed ({error.returncode}): {rendered_command}"
        ) from error


def point_along(
    points: tuple[tuple[float, float], ...], progress: float
) -> tuple[float, float]:
    segments: list[tuple[float, float, float, float, float]] = []
    total = 0.0
    for (x1, y1), (x2, y2) in zip(points, points[1:]):
        length = math.hypot(x2 - x1, y2 - y1)
        segments.append((x1, y1, x2, y2, length))
        total += length

    distance = progress * total
    for x1, y1, x2, y2, length in segments:
        if distance <= length:
            ratio = 0.0 if length == 0 else distance / length
            return x1 + (x2 - x1) * ratio, y1 + (y2 - y1) * ratio
        distance -= length
    return points[-1]


def opacity_at(progress: float) -> float:
    edge = min(progress / 0.1, (1.0 - progress) / 0.1, 1.0)
    return max(0.0, edge)


def particles_for_frame(phase: float, theme: str) -> str:
    core, halo = ("#FFE0B8", "#F0883E") if theme == "dark" else ("#7C2D12", "#B45309")
    circles = ['  <g id="flow-particles" pointer-events="none">']
    for track in FLOW_TRACKS:
        for index in range(track.particles):
            progress = (
                phase * track.cycles + track.offset + index / track.particles
            ) % 1.0
            x, y = point_along(track.points, progress)
            opacity = opacity_at(progress)
            circles.append(
                f'    <circle cx="{x:.2f}" cy="{y:.2f}" r="3.2" '
                f'fill="{halo}" opacity="{0.24 * opacity:.3f}"/>'
            )
            circles.append(
                f'    <circle cx="{x:.2f}" cy="{y:.2f}" r="1.65" '
                f'fill="{core}" opacity="{opacity:.3f}"/>'
            )
    circles.append("  </g>")
    return "\n".join(circles)


def render_gif(source: Path, output: Path, theme: str, temporary: Path) -> None:
    source_text = source.read_text()
    if "</svg>" not in source_text:
        raise RuntimeError(f"not an SVG: {source.relative_to(ROOT)}")

    frames = 72
    fps = 12
    frame_dir = temporary / f"{source.stem}-frames"
    frame_dir.mkdir()

    for index in range(frames):
        phase = index / frames
        particles = particles_for_frame(phase, theme)
        frame_svg = source_text.replace("</svg>", f"{particles}\n</svg>")
        svg_path = frame_dir / f"frame-{index:03d}.svg"
        png_path = frame_dir / f"frame-{index:03d}.png"
        svg_path.write_text(frame_svg)
        run(
            [
                "rsvg-convert",
                "-w",
                "1200",
                "-h",
                "888",
                str(svg_path),
                "-o",
                str(png_path),
            ]
        )

    unoptimized = frame_dir / "unoptimized.gif"
    run(
        [
            "ffmpeg",
            "-hide_banner",
            "-loglevel",
            "error",
            "-framerate",
            str(fps),
            "-i",
            str(frame_dir / "frame-%03d.png"),
            "-filter_complex",
            "[0:v]split[a][b];[a]palettegen=max_colors=128:stats_mode=diff[p];"
            "[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle",
            "-loop",
            "0",
            str(unoptimized),
        ]
    )
    run(
        [
            "gifsicle",
            # FFmpeg already quantizes the frames. Preserve its palette and
            # frame rectangles: reoptimizing can change bytes across gifsicle
            # versions even when every displayed frame remains identical.
            "-O0",
            "--loopcount=0",
            str(unoptimized),
            "-o",
            str(output),
        ]
    )


def render_one(diagram: Diagram, theme: str, output_dir: Path, temporary: Path) -> None:
    source = SOURCE_DIR / f"{diagram.name}-{theme}.svg"
    png_output = output_dir / f"{diagram.name}-{theme}.png"
    run(
        [
            "rsvg-convert",
            "-w",
            str(diagram.width),
            "-h",
            str(diagram.height),
            str(source),
            "-o",
            str(png_output),
        ]
    )
    if diagram.animate:
        render_gif(
            source,
            output_dir / f"{diagram.name}-{theme}.gif",
            theme,
            temporary,
        )


def expected_outputs(output_dir: Path) -> list[Path]:
    outputs = [
        output_dir / f"{diagram.name}-{theme}.png"
        for diagram in DIAGRAMS
        for theme in THEMES
    ]
    outputs.extend(
        output_dir / f"{diagram.name}-{theme}.gif"
        for diagram in DIAGRAMS
        if diagram.animate
        for theme in THEMES
    )
    return outputs


def render_all(output_dir: Path, temporary: Path) -> None:
    output_dir.mkdir()
    jobs = [(diagram, theme) for diagram in DIAGRAMS for theme in THEMES]
    with ThreadPoolExecutor(max_workers=4) as executor:
        futures = [
            executor.submit(render_one, diagram, theme, output_dir, temporary)
            for diagram, theme in jobs
        ]
        for future in futures:
            future.result()


def check_outputs(generated_dir: Path) -> None:
    failures: list[str] = []
    for generated in expected_outputs(generated_dir):
        relative_output = generated.relative_to(generated_dir)
        tracked = OUTPUT_DIR / relative_output
        if not tracked.is_file():
            failures.append(f"missing: {tracked.relative_to(ROOT)}")
        elif generated.read_bytes() != tracked.read_bytes():
            failures.append(f"out of date: {tracked.relative_to(ROOT)}")

    if failures:
        details = "\n".join(f"  - {failure}" for failure in failures)
        raise SystemExit(
            f"README diagram assets do not match their sources:\n{details}\n"
            "Run scripts/render-readme-diagrams.py and review the generated assets."
        )

    print("README diagram assets match their tracked sources.")


def install_outputs(generated_dir: Path) -> None:
    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)
    for generated in expected_outputs(generated_dir):
        shutil.copyfile(generated, OUTPUT_DIR / generated.name)
    print(f"Rendered README diagram assets in {OUTPUT_DIR.relative_to(ROOT)}/.")


def main() -> None:
    args = parse_args()
    validate_inputs()
    with tempfile.TemporaryDirectory(prefix="fluent-readme-diagrams-") as directory:
        temporary = Path(directory)
        generated_dir = temporary / "generated"
        render_all(generated_dir, temporary)
        if args.check:
            check_outputs(generated_dir)
        else:
            install_outputs(generated_dir)


if __name__ == "__main__":
    try:
        main()
    except RuntimeError as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1) from error
