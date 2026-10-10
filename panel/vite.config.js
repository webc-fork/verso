import { dirname, join } from 'node:path';
import { existsSync, copyFileSync, unlinkSync } from 'node:fs';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { viteSingleFile } from 'vite-plugin-singlefile';
import sveltePreprocess from 'svelte-preprocess';
import read from '@3-/read';
import write from '@3-/write';
import stylus from 'stylus';
import minifyHtml from '@minify-html/node';
import { optimize } from 'svgo';

const PWD = import.meta.dirname,
  ROOT = dirname(PWD);

const svgoPlugin = () => ({
  name: 'vite-plugin-svgo-optimize',
  enforce: 'pre',
  async load(id) {
    const [pathname] = id.split('?');
    if (pathname.endsWith('.svg')) {
      this.addWatchFile(pathname);
      const raw = await read(pathname);
      const result = optimize(raw, {
        multipass: true,
        plugins: [
          'preset-default',
          {
            name: 'removeAttrs',
            params: {
              attrs: '(class)',
            },
          },
        ],
      });
      return `export default ${JSON.stringify(result.data)};`;
    }
  },
});

const stylusPlugin = () => ({
  name: 'vite-plugin-custom-stylus',
  enforce: 'pre',
  async resolveId(source, importer) {
    const [pathname, query] = source.split('?');
    if (pathname.endsWith('.styl')) {
      const resolved = await this.resolve(source, importer, { skipSelf: true });
      if (resolved) {
        const [res_path, res_query] = resolved.id.split('?'),
          suffix = query || res_query,
          target_path = res_path.endsWith('.css') ? res_path : res_path + '.css';
        return suffix ? target_path + '?' + suffix : target_path;
      }
    }
  },
  async load(id) {
    const [pathname] = id.split('?');
    if (pathname.endsWith('.styl.css')) {
      const original_path = pathname.slice(0, -4);
      this.addWatchFile(original_path);

      const content = await read(original_path),
        renderer = stylus(content)
          .set('filename', original_path)
          .set('sourcemap', { inline: false }),
        css = renderer.render();

      return {
        code: css,
        map: renderer.sourcemap,
      };
    }
  },
  handleHotUpdate({ file, server }) {
    if (file.endsWith('.styl') || file.endsWith('.pug')) {
      const { moduleGraph, ws } = server,
        file_mods = moduleGraph.getModulesByFile(file);

      for (const [id, mod] of moduleGraph.idToModuleMap.entries()) {
        if (id.includes(file)) moduleGraph.invalidateModule(mod);
      }
      if (file_mods) {
        file_mods.forEach((mod) => moduleGraph.invalidateModule(mod));
      }
      ws.send({ type: 'full-reload' });
      return [];
    }
  },
});

const emitPanelHtml = () => ({
  name: 'emit-panel-html',
  enforce: 'post',
  closeBundle() {
    const src = join(ROOT, 'resources/components/index.html'),
      dest = join(ROOT, 'resources/components/panel.html');

    if (existsSync(src)) {
      const rawHtml = read(src);
      try {
        const minified = minifyHtml.minify(Buffer.from(rawHtml), {
          minify_js: true,
          minify_css: true,
        });
        write(dest, minified);
        console.log(`Successfully generated resources/components/panel.html (${minified.length} bytes)`);
      } catch (err) {
        console.warn('HTML minification error, using unminified HTML:', err);
        copyFileSync(src, dest);
      }
      unlinkSync(src);
    }
  },
});

export default defineConfig({
  plugins: [
    svgoPlugin(),
    stylusPlugin(),
    svelte({
      preprocess: sveltePreprocess({
        pug: true,
        stylus: true,
      }),
    }),
    viteSingleFile(),
    emitPanelHtml(),
  ],
  esbuild: {
    drop: ['debugger'],
  },
  css: {
    transformer: 'lightningcss',
    lightningcss: {
      targets: {
        chrome: 90 << 16,
      },
    },
  },
  build: {
    target: 'esnext',
    cssMinify: 'lightningcss',
    outDir: join(ROOT, 'resources/components'),
    emptyOutDir: false,
  },
});
