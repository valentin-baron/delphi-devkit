const esbuild = require("esbuild");
const fs = require('fs');
const path = require('path');

const production = process.argv.includes('--production');
const watch = process.argv.includes('--watch');

function copyAsset(source) {
	const dest = path.join(__dirname, 'dist', path.basename(source));
	try {
		fs.copyFileSync(source, dest);
		console.log(`✓ Copied ${path.basename(source)} to dist/`);
	} catch (error) {
		console.error(`✘ Failed to copy ${path.basename(source)}:`, error.message);
	}
}

/**
 * @type {import('esbuild').Plugin}
 */
const copyAssetsPlugin = {
	name: 'copy-assets',
	setup(build) {
		build.onEnd(() => {
			// Copy PowerShell script to dist folder
			const sourceScript = path.join(__dirname, 'src', 'projects', 'compiler', 'compile.ps1');
			copyAsset(sourceScript);

			const sourceWasm = path.join(__dirname, 'node_modules', 'sql.js', 'dist', 'sql-wasm.wasm');
			copyAsset(sourceWasm);

			const sourceLicense = path.join(__dirname, 'LICENSE');
			copyAsset(sourceLicense);

			const sourceNotice = path.join(__dirname, 'NOTICE.txt');
			copyAsset(sourceNotice);

			const readmeFile = path.join(__dirname, 'README.md');
			copyAsset(readmeFile);

			const changelog = path.join(__dirname, 'CHANGELOG.md');
			copyAsset(changelog);

			const formatterConfig = path.join(__dirname, 'config', 'ddk_formatter.config');
			copyAsset(formatterConfig);
		});
	},
};

/**
 * @type {import('esbuild').Plugin}
 */
const esbuildProblemMatcherPlugin = {
	name: 'esbuild-problem-matcher',

	setup(build) {
		build.onStart(() => {
			console.log('[watch] build started');
		});
		build.onEnd((result) => {
			result.errors.forEach(({ text, location }) => {
				console.error(`✘ [ERROR] ${text}`);
				console.error(`    ${location.file}:${location.line}:${location.column}:`);
			});
			console.log('[watch] build finished');
		});
	},
};

async function main() {
	const ctx = await esbuild.context({
		entryPoints: [
			'src/extension.ts'
		],
		bundle: true,
		format: 'cjs',
		minify: production,
		sourcemap: !production,
		sourcesContent: false,
		platform: 'node',
		outfile: 'dist/extension.js',
		external: ['vscode'],
		logLevel: 'silent',
		plugins: [
			copyAssetsPlugin,
			/* add to the end of plugins array */
			esbuildProblemMatcherPlugin,
		],
	});
	if (watch) {
		await ctx.watch();
	} else {
		await ctx.rebuild();
		await ctx.dispose();
	}
}

main().catch(e => {
	console.error(e);
	process.exit(1);
});
