export const captureColors: Record<string, [string, string]> = {
  black: ['#111214', '#11161b'],
  red: ['#a5251c', '#ff7b72'],
  green: ['#25733a', '#7ee787'],
  yellow: ['#805600', '#e3b341'],
  blue: ['#315eaa', '#79c0ff'],
  magenta: ['#833a9c', '#d2a8ff'],
  cyan: ['#13747c', '#56d4dd'],
  gray: ['#5e615e', '#8c949e'],
  'dark-gray': ['#5e615e', '#8c949e'],
  white: ['#111214', '#f0f2f6'],
  '#000000': ['#000000', '#000000'],
  '#f0f2f6': ['#111214', '#f0f2f6'],
  '#8c8c8c': ['#5e615e', '#8c8c8c'],
  '#5b8def': ['#315eaa', '#5b8def'],
  '#9b89ac': ['#6f5c80', '#9b89ac'],
  '#b87e54': ['#87532f', '#b87e54'],
  '#c678dd': ['#833a9c', '#c678dd'],
  '#e5a84b': ['#805600', '#e5a84b'],
};

const lightBackgrounds: Record<string, string> = {
  '#2c323c': '#e4e6e7',
  '#5b8def': '#b9cdf2',
  '#c678dd': '#dfb9e8',
  '#e5a84b': '#f1d59a',
  cyan: '#a8e3e7',
};

const pillCap = /^[]+$/;

export const captureColor = (value: string, text = '') =>
  value === 'reset'
    ? undefined
    : `light-dark(${pillCap.test(text) ? lightBackgrounds[value] ?? captureColors[value]?.[0] ?? value : captureColors[value]?.[0] ?? value},${captureColors[value]?.[1] ?? value})`;

export const captureBackground = (value: string) =>
  value === 'reset'
    ? undefined
    : `light-dark(${lightBackgrounds[value] ?? value},${captureColors[value]?.[1] ?? value})`;
