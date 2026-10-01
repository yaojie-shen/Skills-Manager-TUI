import { createHash } from 'node:crypto';
import { mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { basename, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const outputDir = resolve(root, 'public/fonts');
const fonts = [
  {
    file: 'Silkscreen-Regular.ttf',
    url: 'https://raw.githubusercontent.com/googlefonts/silkscreen/206ccf3f5234c281461e63ecc59cbc6b0563472b/fonts/ttf/Silkscreen-Regular.ttf',
    sha256: 'c845473330b94c2079ce9af01c51ac8ba2d99c24f4d14c039843bbb8e642ebd8',
  },
  {
    file: 'JetBrainsMonoNerdFontMono-Regular.ttf',
    url: 'https://raw.githubusercontent.com/ryanoasis/nerd-fonts/v3.4.0/patched-fonts/JetBrainsMono/Ligatures/Regular/JetBrainsMonoNerdFontMono-Regular.ttf',
    sha256: 'f01031f40e48dc29e1112e6b0b0450a2c6cd097f3f35cfff05c55cb311f8034c',
  },
  {
    file: 'JetBrainsMonoNerdFontMono-Bold.ttf',
    url: 'https://raw.githubusercontent.com/ryanoasis/nerd-fonts/v3.4.0/patched-fonts/JetBrainsMono/Ligatures/Bold/JetBrainsMonoNerdFontMono-Bold.ttf',
    sha256: '5bdd4a873f3cd32f882d2c55545089123926e27707d5880fc9eaf84eb01b6686',
  },
];

const digest = (bytes) => createHash('sha256').update(bytes).digest('hex');

async function valid(path, sha256) {
  try {
    return digest(await readFile(path)) === sha256;
  } catch (error) {
    if (error?.code === 'ENOENT') return false;
    throw error;
  }
}

await mkdir(outputDir, { recursive: true });

for (const font of fonts) {
  const output = resolve(outputDir, font.file);
  if (await valid(output, font.sha256)) {
    console.log(`font cached: ${font.file}`);
    continue;
  }

  const temporary = `${output}.${process.pid}.tmp`;
  try {
    const response = await fetch(font.url, { redirect: 'follow' });
    if (!response.ok) {
      throw new Error(`HTTP ${response.status} ${response.statusText}`);
    }

    const bytes = Buffer.from(await response.arrayBuffer());
    const received = digest(bytes);
    if (received !== font.sha256) {
      throw new Error(`checksum mismatch (expected ${font.sha256}, received ${received})`);
    }

    await writeFile(temporary, bytes);
    await rename(temporary, output);
    console.log(`font downloaded: ${basename(output)}`);
  } catch (error) {
    await rm(temporary, { force: true });
    throw new Error(`Failed to fetch ${font.file} from ${font.url}: ${error.message}`);
  }
}
