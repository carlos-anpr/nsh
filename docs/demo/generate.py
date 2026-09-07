"""Renderiza una demo ilustrativa sin acceder a la shell ni a credenciales.

Requiere Pillow: python3 docs/demo/generate.py
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


OUT = Path(__file__).resolve().parent
FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
REGULAR = ImageFont.truetype(FONT, 21)
SMALL = ImageFont.truetype(FONT, 16)
TITLE = ImageFont.truetype(FONT, 30)
BG, PANEL = "#0c1220", "#151e30"
TEXT, MUTED, GREEN, BLUE = "#e3eaf6", "#9aabc4", "#77e5b3", "#89bcff"
PROMPT = "nsh ~/demo ❯ "
FPS = 10
DURATION = 15

# Guion fijo: rutas, ficheros y respuestas ficticias, nunca datos del equipo.
SCENES = [
    (0, 2.5, "01 / Explora los datos", "! ejecuta comandos Bash directamente", "!ls logs/", 0.5,
     [("access.jsonl", TEXT), ("[terminado: 0]", GREEN)]),
    (2.5, 10.5, "02 / Una búsqueda con varios filtros", "El LLM combina filtros, agrupación y ordenación",
     "Busca errores 500 de /api/pagos en logs/access.jsonl.\nExcluye pruebas y muestra las 3 IP con más errores.", 2.0,
     [("  Filtra los registros y cuenta los errores por IP.", TEXT),
      ("  $ jq -r 'select(.status == 500 and", BLUE),
      ('      .path == "/api/pagos" and .env != "test") | .ip\'', BLUE),
      ("      logs/access.jsonl | sort | uniq -c | sort -nr | head -3", BLUE),
      ("      8 192.0.2.10", TEXT),
      ("      3 192.0.2.20", TEXT),
      ("      1 192.0.2.30", TEXT),
      ("[terminado: 0]  · /why para interpretar la salida", GREEN)]),
    (10.5, 15, "03 / De la búsqueda a la respuesta", "/why interpreta el resultado y explica los filtros", "/why", 0.5,
     [("Encontrados 12 errores 500 de /api/pagos.", TEXT),
      ("192.0.2.10 concentra 8 de los 12 errores (67 %).", GREEN),
      ("Las otras IP tienen 3 y 1 errores, respectivamente.", TEXT),
      ("", TEXT),
      ("Se excluyeron los registros del entorno test.", TEXT),
      ("El resultado está agrupado por IP, de mayor a menor.", TEXT)]),
]


def frame(t):
    canvas = Image.new("RGB", (1000, 660), BG)
    d = ImageDraw.Draw(canvas)
    scene = next(s for s in SCENES if s[0] <= t < s[1])
    start, end, label, subtitle, command, typing, lines = scene
    elapsed = t - start
    d.text((36, 22), "nsh", font=TITLE, fill=TEXT)
    d.text((145, 34), "Tu terminal, en lenguaje natural", font=SMALL, fill=MUTED)
    d.text((36, 79), label, font=REGULAR, fill=GREEN)
    d.text((36, 113), subtitle, font=SMALL, fill=MUTED)
    d.rounded_rectangle((28, 156, 972, 589), radius=15, fill=PANEL)
    for i, color in enumerate(("#ff7b86", "#ffd480", "#77e5b3")):
        x = 50 + i * 23
        d.ellipse((x, 174, x + 10, 184), fill=color)
    d.text((139, 170), "nsh — sesión de ejemplo", font=SMALL, fill=MUTED)
    d.line((44, 204, 956, 204), fill="#29344a")
    count = min(len(command), int(max(0, elapsed - 0.3) / typing * len(command)))
    typed = command[:count]
    d.text((48, 225), PROMPT, font=REGULAR, fill=GREEN)
    x = 48 + d.textlength(PROMPT, font=REGULAR)
    typed_lines = typed.split("\n")
    for row, part in enumerate(typed_lines):
        d.text((x if row == 0 else 48, 225 + row * 31), part, font=REGULAR, fill=TEXT)
    output_y = 264 + 31 * command.count("\n")
    ready = 0.3 + typing
    if elapsed < ready:
        if int(t * 3) % 2 == 0:
            cursor = (x if len(typed_lines) == 1 else 48) + d.textlength(typed_lines[-1], font=REGULAR)
            y = 226 + 31 * (len(typed_lines) - 1)
            d.rectangle((cursor + 1, y, cursor + 11, y + 24), fill=GREEN)
    else:
        delay = 0 if start == 0 else 0.6
        if elapsed < ready + delay:
            status = "pensando…" if start == 2.5 else "explicando…"
            d.text((48, output_y), status, font=REGULAR, fill=MUTED)
        else:
            for i, (line, color) in enumerate(lines):
                d.text((48, output_y + i * 31), line, font=REGULAR, fill=color)
            d.text((48, output_y + len(lines) * 31), PROMPT + "▏", font=REGULAR, fill=GREEN)
    d.text((36, 608), "Demo ilustrativa · datos ficticios", font=SMALL, fill=MUTED)
    d.text((645, 608), "! Bash   /   texto LLM   /   /why", font=SMALL, fill=BLUE)
    d.rectangle((28, 646, 972, 650), fill=PANEL)
    d.rectangle((28, 646, 28 + int(944 * t / DURATION), 650), fill=GREEN)
    return canvas


if __name__ == "__main__":
    images = [frame(i / FPS) for i in range(FPS * DURATION)]
    palette = images[-1].quantize(colors=96)
    frames = [im.quantize(palette=palette, dither=Image.Dither.NONE) for im in images]
    frames[0].save(OUT / "nsh-demo.gif", save_all=True, append_images=frames[1:],
                   duration=100, loop=0, optimize=True, disposal=1)
    images[140].save(OUT / "nsh-demo-preview.png")
    print("Demo generada: docs/demo/nsh-demo.gif (15 segundos)")
