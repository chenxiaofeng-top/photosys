const units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
const formatter = new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 2 });

export function formatBytes(bytes: number) {
  let unit = 0;
  while (bytes >= 1024 && unit < units.length - 1) {
    bytes /= 1024;
    unit++;
  }
  return `${formatter.format(bytes)} ${units[unit]}`;
}
