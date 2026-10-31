"""Bounded cell decoder for Shellboy's VT output; not a general terminal emulator."""
from collections import namedtuple
import unicodedata

Cell = namedtuple('Cell', 'data fg bg', defaults=(' ', 'default', 'default'))
ANSI = ('000000', 'cd0000', '00cd00', 'cdcd00', '0000ee', 'cd00cd', '00cdcd',
        'e5e5e5', '7f7f7f', 'ff0000', '00ff00', 'ffff00', '5c5cff', 'ff00ff',
        '00ffff', 'ffffff')


def indexed_color(index):
    if not 0 <= index <= 255:
        raise ValueError('Invalid indexed color')
    if index < 16:
        return ANSI[index]
    if index >= 232:
        return f'{8 + 10 * (index - 232):02x}' * 3
    index -= 16
    levels = (0, 95, 135, 175, 215, 255)
    return ''.join(f'{levels[n]:02x}' for n in (index // 36, index // 6 % 6, index % 6))


class Screen:
    def __init__(self, columns, lines):
        if not 1 <= columns <= 400 or not 1 <= lines <= 200:
            raise ValueError('Screen must be 1..400 columns and 1..200 rows')
        self.columns, self.lines = columns, lines
        self.state, self.sequence = 'text', ''
        self.normal = None
        self.reset()

    def reset(self):
        self.fg = self.bg = 'default'
        self.wrap = True
        self.buffer = [[Cell()] * self.columns for _ in range(self.lines)]
        self.x = self.y = 0
        self.saved = (0, 0)
        self.last = None

    def text(self):
        return '\n'.join(''.join(cell.data for cell in row) for row in self.buffer)

    def blank(self):
        return Cell(' ', self.fg, self.bg)

    def clear_cell(self, y, x):
        if self.buffer[y][x].data == '' and x:
            self.buffer[y][x - 1] = self.blank()
        elif x + 1 < self.columns and self.buffer[y][x + 1].data == '':
            self.buffer[y][x + 1] = self.blank()
        self.buffer[y][x] = self.blank()

    def newline(self):
        self.last = None
        if self.y == self.lines - 1:
            self.buffer.pop(0)
            self.buffer.append([self.blank()] * self.columns)
        else:
            self.y += 1

    def write(self, char):
        code = ord(char)
        combining = unicodedata.category(char) in ('Mn', 'Me', 'Cf') or 0x1F3FB <= code <= 0x1F3FF
        previous = self.buffer[self.last[0]][self.last[1]] if self.last else None
        if previous and char in ('\ufe0e', '\ufe0f') and not (
                self.last[1] + 1 < self.columns and self.buffer[self.last[0]][self.last[1] + 1].data == ''):
            raise ValueError('Narrow-base emoji presentation widths are unsupported')
        if previous and (combining or previous.data.endswith('\u200d')):
            if len(previous.data) >= 64:
                raise ValueError('Cell cluster exceeds 64 codepoints')
            y, x = self.last
            self.buffer[y][x] = previous._replace(data=previous.data + char)
            return
        if combining:
            return
        width = 2 if unicodedata.east_asian_width(char) in ('W', 'F') else 1
        if self.x + width > self.columns:
            if self.wrap:
                self.x = 0
                self.newline()
            else:
                self.x = max(0, self.columns - width)
        if width > self.columns:
            raise ValueError('Wide glyph cannot fit this screen')
        self.clear_cell(self.y, self.x)
        if width == 2:
            self.clear_cell(self.y, self.x + 1)
        self.buffer[self.y][self.x] = Cell(char, self.fg, self.bg)
        self.last = (self.y, self.x)
        if width == 2:
            self.buffer[self.y][self.x + 1] = Cell('', self.fg, self.bg)
        self.x += width

    def sgr(self, values):
        index = 0
        while index < len(values):
            value = values[index]
            if value == 0:
                self.fg = self.bg = 'default'
            elif value == 39:
                self.fg = 'default'
            elif value == 49:
                self.bg = 'default'
            elif 30 <= value <= 37 or 90 <= value <= 97:
                self.fg = indexed_color(value - 30 if value < 90 else value - 90 + 8)
            elif 40 <= value <= 47 or 100 <= value <= 107:
                self.bg = indexed_color(value - 40 if value < 100 else value - 100 + 8)
            elif value in (38, 48, 58):
                if index + 2 < len(values) and values[index + 1] == 5:
                    color = indexed_color(values[index + 2])
                    index += 2
                elif index + 4 < len(values) and values[index + 1] == 2:
                    rgb = values[index + 2:index + 5]
                    if any(not 0 <= channel <= 255 for channel in rgb):
                        raise ValueError('Invalid true color')
                    color = ''.join(f'{channel:02x}' for channel in rgb)
                    index += 4
                else:
                    raise ValueError('Incomplete color sequence')
                if value == 38:
                    self.fg = color
                elif value == 48:
                    self.bg = color
            # Bold/italic/underline and underline color do not move cells.
            index += 1

    def csi(self, sequence):
        command, raw = sequence[-1], sequence[:-1]
        if raw.startswith('?') and command in 'hl':
            modes = [int(part) for part in raw[1:].split(';')]
            if set(modes) - {1, 7, 12, 25, 1000, 1002, 1003, 1004, 1005, 1006, 1015, 1049, 2004, 2026}:
                raise ValueError('Unsupported private terminal mode')
            for mode in modes:
                if mode == 7:
                    self.wrap = command == 'h'
                elif mode == 1049 and command == 'h' and self.normal is None:
                    self.normal = (self.buffer, self.x, self.y, self.fg, self.bg, self.saved, self.wrap)
                    self.reset()
                elif mode == 1049 and command == 'l' and self.normal is not None:
                    self.buffer, self.x, self.y, self.fg, self.bg, self.saved, self.wrap = self.normal
                    self.normal, self.last = None, None
            return
        if raw and any(char not in '0123456789;' for char in raw):
            raise ValueError('Unsupported CSI parameters: ' + repr(sequence))
        values = [int(part or 0) for part in raw.split(';')]
        count = values[0] or 1
        x = min(self.x, self.columns - 1)
        if command == 'm':
            self.sgr(values)
            return
        if command in 'Hf':
            self.y = min(self.lines - 1, max(0, values[0] - 1))
            self.x = min(self.columns - 1, max(0, (values[1] if len(values) > 1 else 1) - 1))
        elif command in 'ABCD':
            self.y = max(0, self.y - count) if command == 'A' else min(self.lines - 1, self.y + count) if command == 'B' else self.y
            self.x = min(self.columns - 1, x + count) if command == 'C' else max(0, x - count) if command == 'D' else x
        elif command in 'G`':
            self.x = min(self.columns - 1, count - 1)
        elif command == 'd':
            self.y = min(self.lines - 1, count - 1)
        elif command in 'JKX':
            mode = values[0]
            if command == 'X':
                cells = [(self.y, col) for col in range(x, min(self.columns, x + count))]
            elif command == 'K':
                cells = [(self.y, col) for col in range(self.columns) if mode == 2 or mode == 0 and col >= x or mode == 1 and col <= x]
            elif mode in (2, 3):
                cells = [(row, col) for row in range(self.lines) for col in range(self.columns)]
            else:
                cells = [(row, col) for row in range(self.lines) for col in range(self.columns) if mode == 0 and (row, col) >= (self.y, x) or mode == 1 and (row, col) <= (self.y, x)]
            for row, col in cells:
                self.clear_cell(row, col)
        elif command == 's':
            self.saved = (self.x, self.y)
        elif command == 'u':
            self.x, self.y = self.saved
        elif command not in ('n', 'c', 't'):
            raise ValueError('Unsupported cell-changing CSI: ' + repr(sequence))
        self.last = None

    def feed(self, text):
        for char in text:
            if self.state == 'osc':
                if char == '\a':
                    self.state = 'text'
                elif char == '\x1b':
                    self.state = 'osc_escape'
            elif self.state == 'osc_escape':
                self.state = 'text' if char == '\\' else 'osc'
            elif self.state == 'charset':
                if char != 'B':
                    raise ValueError('Only ASCII charset is supported')
                self.state = 'text'
            elif self.state == 'escape':
                self.state = 'text'
                if char == '[':
                    self.state, self.sequence = 'csi', ''
                elif char == ']':
                    self.state = 'osc'
                elif char in '()':
                    self.state = 'charset'
                elif char == '7':
                    self.saved = (self.x, self.y)
                elif char == '8':
                    self.x, self.y = self.saved
                elif char == 'c':
                    self.reset()
                else:
                    raise ValueError('Unsupported escape: ' + repr(char))
            elif self.state == 'csi':
                self.sequence += char
                if len(self.sequence) > 512:
                    raise ValueError('CSI exceeds 512 characters')
                if '@' <= char <= '~':
                    self.csi(self.sequence)
                    self.state = 'text'
            elif char == '\x1b':
                self.state = 'escape'
            elif char == '\r':
                self.x, self.last = 0, None
            elif char == '\n':
                self.newline()
            elif char == '\b':
                self.x, self.last = max(0, self.x - 1), None
            elif char == '\t':
                self.x, self.last = min(self.columns - 1, (self.x // 8 + 1) * 8), None
            elif ord(char) >= 32 and char != '\x7f':
                self.write(char)
