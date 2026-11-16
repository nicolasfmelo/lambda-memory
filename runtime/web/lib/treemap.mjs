// Squarified rows; retain the input order while assigning small empty roots a share.
export function squarify(items, width, height, minShare = 0.04) {
  if (!items.length || width <= 0 || height <= 0)
    return [];
  const values = items.map(i => Math.max(0, Number(i.value) || 0));
  const sum = values.reduce((a, b) => a + b, 0);
  const zeros = values.filter(v => !v).length;
  const zeroShare = sum ? Math.min(Math.max(0, minShare), 1 / items.length) : 1 / items.length;
  const areas = values.map(
      v => (sum ? (v ? v / sum * (1 - zeros * zeroShare) : zeroShare) : 1 / items.length) * width *
          height);
  const result = [];
  let x = 0;
  let y = 0;
  let w = width;
  let h = height;
  let index = 0;
  const worst = (row, side) => {
    const s = row.reduce((a, b) => a + b, 0);
    return Math.max(
        side * side * Math.max(...row) / (s * s), s * s / (side * side * Math.min(...row)));
  };
  while (index < items.length) {
    const side = Math.min(w, h);
    const row = [areas[index]];
    while (index + row.length < items.length &&
           worst([...row, areas[index + row.length]], side) <= worst(row, side))
      row.push(areas[index + row.length]);
    const total = row.reduce((a, b) => a + b, 0);
    let pos = 0;
    if (w >= h) {
      const rw = total / h;
      row.forEach((a, j) => {
        const rh = j === row.length - 1 ? h - pos : a / rw;
        result.push({key: items[index + j].key, x, y: y + pos, w: rw, h: rh});
        pos += rh;
      });
      x += rw;
      w -= rw;
    } else {
      const rh = total / w;
      row.forEach((a, j) => {
        const rw = j === row.length - 1 ? w - pos : a / rh;
        result.push({key: items[index + j].key, x: x + pos, y, w: rw, h: rh});
        pos += rw;
      });
      y += rh;
      h -= rh;
    }
    index += row.length;
  }
  return result;
}
