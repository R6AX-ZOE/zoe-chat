// 将 static/ 下的静态资源复制到 dist/(与 tsc 输出合并)
import { cpSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
mkdirSync(join(root, 'dist'), { recursive: true });
cpSync(join(root, 'static'), join(root, 'dist'), { recursive: true });
console.log('static assets copied to dist/');
