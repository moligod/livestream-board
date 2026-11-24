"""Small QR Code Model 2 encoder for local login URLs (ISO/IEC 18004).

Uses byte mode and error-correction level M. No image/network dependencies.
"""

from __future__ import annotations

from itertools import zip_longest

# Version, data codewords per Reed–Solomon block, parity per block,
# and alignment centers. Use a bounded set of symbol sizes for the login UI.
_LAYOUTS = (
    (4, (32, 32), 18, (6, 26)),
    (6, (27,) * 4, 16, (6, 34)),
    (8, (38, 38, 39, 39), 22, (6, 24, 42)),
    (10, (43,) * 4 + (44,), 26, (6, 28, 50)),
    (12, (36,) * 6 + (37,) * 2, 22, (6, 32, 58)),
    (14, (40,) * 4 + (41,) * 5, 24, (6, 26, 46, 66)),
)


def _gf_times(left: int, right: int) -> int:
    product = 0
    while right:
        if right & 1:
            product ^= left
        right >>= 1
        left <<= 1
        if left & 0x100:
            left ^= 0x11D
    return product


def _parity(block: bytes, width: int) -> list[int]:
    polynomial = [1]
    root = 1
    for _ in range(width):
        scaled = [_gf_times(root, term) for term in polynomial]
        polynomial = [a ^ b for a, b in zip(polynomial + [0], [0] + scaled)]
        root = _gf_times(root, 2)
    work = list(block) + [0] * width
    for offset in range(len(block)):
        lead = work[offset]
        if lead:
            for index, coefficient in enumerate(polynomial):
                work[offset + index] ^= _gf_times(lead, coefficient)
    return work[-width:]


def _interleave(bits: str, block_sizes: tuple[int, ...], parity_width: int) -> bytes:
    limit = 8 * sum(block_sizes)
    bits += '0' * min(4, limit - len(bits))
    bits += '0' * (-len(bits) % 8)
    words = bytes(int(bits[i:i + 8], 2) for i in range(0, len(bits), 8))
    pad_length = sum(block_sizes) - len(words)
    words += (b'\xec\x11' * ((pad_length + 1) // 2))[:pad_length]
    blocks = []
    start = 0
    for length in block_sizes:
        blocks.append(words[start:start + length])
        start += length
    data = (word for column in zip_longest(*blocks) for word in column if word is not None)
    checks = (word for column in zip(*(_parity(block, parity_width) for block in blocks)) for word in column)
    return bytes(data) + bytes(checks)


def _write_bch(value: int, degree: int, divisor: int) -> int:
    remainder = value << degree
    while remainder.bit_length() > degree:
        remainder ^= divisor << (remainder.bit_length() - degree - 1)
    return (value << degree) | remainder


def _format_cells(size: int) -> tuple[list[tuple[int, int]], list[tuple[int, int]]]:
    first = [(r, 8) for r in range(6)] + [(7, 8), (8, 8), (8, 7)]
    first.extend((8, c) for c in range(5, -1, -1))
    second = [(8, c) for c in range(size - 1, size - 9, -1)]
    second.extend((r, 8) for r in range(size - 7, size))
    return first, second


def _function_modules(version: int, centers: tuple[int, ...]) -> list[list[int | None]]:
    size = 17 + 4 * version
    matrix: list[list[int | None]] = [[None] * size for _ in range(size)]
    for row, col in ((3, 3), (3, size - 4), (size - 4, 3)):
        for r in range(max(0, row - 4), min(size, row + 5)):
            for c in range(max(0, col - 4), min(size, col + 5)):
                radius = max(abs(r - row), abs(c - col))
                matrix[r][c] = int(radius <= 3 and radius != 2)
    for i in range(8, size - 8):
        matrix[6][i] = matrix[i][6] = int(i % 2 == 0)
    finder_corners = {(6, 6), (6, centers[-1]), (centers[-1], 6)}
    for row in centers:
        for col in centers:
            if (row, col) not in finder_corners:
                for y in range(-2, 3):
                    for x in range(-2, 3):
                        matrix[row + y][col + x] = int(max(abs(y), abs(x)) != 1)
    for group in _format_cells(size):
        for r, c in group:
            matrix[r][c] = 0
    matrix[size - 8][8] = 1
    if version >= 7:
        packed = _write_bch(version, 12, 0x1F25)
        for shift in range(18):
            r, c = divmod(shift, 3)
            c += size - 11
            matrix[r][c] = matrix[c][r] = (packed >> shift) & 1
    return matrix


def _data_cells(matrix: list[list[int | None]]):
    size = len(matrix)
    right = size - 1
    upwards = True
    while right > 0:
        if right == 6:
            right -= 1
        rows = range(size - 1, -1, -1) if upwards else range(size)
        for r in rows:
            for c in (right, right - 1):
                if matrix[r][c] is None:
                    yield r, c
        right -= 2
        upwards = not upwards


def _flip(mask: int, row: int, col: int) -> bool:
    product = row * col
    return (
        (row + col) % 2, row % 2, col % 3, (row + col) % 3,
        (row // 2 + col // 3) % 2, product % 2 + product % 3,
        (product % 2 + product % 3) % 2, ((row + col) % 2 + product % 3) % 2,
    )[mask] == 0


def _penalty(matrix: list[list[int]]) -> int:
    score = 0
    for line in (*matrix, *zip(*matrix)):
        run = 0
        last = -1
        for cell in line:
            run = run + 1 if cell == last else 1
            score += 3 if run == 5 else int(run > 5)
            last = cell
        for i in range(len(line) - 10):
            if tuple(line[i:i + 11]) in ((0, 0, 0, 0, 1, 0, 1, 1, 1, 0, 1), (1, 0, 1, 1, 1, 0, 1, 0, 0, 0, 0)):
                score += 40
    size = len(matrix)
    for r in range(size - 1):
        for c in range(size - 1):
            if matrix[r][c] == matrix[r][c + 1] == matrix[r + 1][c] == matrix[r + 1][c + 1]:
                score += 3
    dark = sum(sum(row) for row in matrix)
    return score + 10 * (abs(20 * dark - 10 * size * size) // (size * size))


def _matrix(text: str) -> list[list[int]]:
    data = text.encode('utf-8')
    if len(data) > 362:
        raise ValueError('Login QR input exceeds the supported byte capacity')
    # ECI assignment 26 declares UTF-8 for readers when needed.
    prefix = '' if data.isascii() else '011100011010'
    payload = ''.join(f'{byte:08b}' for byte in data)
    for version, sizes, parity_width, centers in _LAYOUTS:
        count_width = 8 if version < 10 else 16
        if len(data) >= (1 << count_width):
            continue
        bits = f'{prefix}0100{len(data):0{count_width}b}{payload}'
        if len(bits) <= 8 * sum(sizes):
            break
    else:
        raise ValueError('Login QR input exceeds the supported byte capacity')
    stream = ''.join(f'{word:08b}' for word in _interleave(bits, sizes, parity_width))
    reserved = _function_modules(version, centers)
    size = len(reserved)
    cells = tuple(_data_cells(reserved))
    if len(stream) > len(cells):
        raise ValueError('Invalid QR layout')
    modules = [int(bit) for bit in stream.ljust(len(cells), '0')]
    alternatives = []
    for mask in range(8):
        picture = [[int(cell or 0) for cell in row] for row in reserved]
        for (r, c), bit in zip(cells, modules):
            picture[r][c] = bit ^ _flip(mask, r, c)
        fmt = _write_bch(mask, 10, 0x537) ^ 0x5412  # M has format indicator 00.
        for group in _format_cells(size):
            for shift, (r, c) in enumerate(group):
                picture[r][c] = (fmt >> shift) & 1
        alternatives.append(picture)
    return min(alternatives, key=_penalty)


def svg(text: str) -> bytes:
    """Encode text as an opaque black-on-white SVG with four quiet modules."""
    matrix = _matrix(text)
    size = len(matrix) + 8
    path = ''.join(
        f'M{col + 4} {row + 4}h1v1h-1z'
        for row, cells in enumerate(matrix) for col, dark in enumerate(cells) if dark
    )
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {size} {size}" shape-rendering="crispEdges">'
        f'<rect width="{size}" height="{size}" fill="#fff"/><path d="{path}" fill="#000"/></svg>'
    ).encode('ascii')
