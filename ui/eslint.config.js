import prettier from 'eslint-config-prettier';
import path from 'node:path';
import js from '@eslint/js';
import svelte from 'eslint-plugin-svelte';
import { defineConfig, includeIgnoreFile } from 'eslint/config';
import globals from 'globals';
import ts from 'typescript-eslint';
import svelteConfig from './svelte.config.js';

const gitignorePath = path.resolve(import.meta.dirname, '.gitignore');

export default defineConfig(
	includeIgnoreFile(gitignorePath),
	{ ignores: ['src/lib/components/ui/**', 'src/lib/hooks/**'] },
	js.configs.recommended,
	ts.configs.recommended,
	svelte.configs.recommended,
	prettier,
	svelte.configs.prettier,
	{
		languageOptions: { globals: { ...globals.browser, ...globals.node } },
		rules: {
			'no-undef': 'off',
			'no-restricted-imports': [
				'error',
				{
					paths: [
						{
							name: 'svelte',
							importNames: ['createEventDispatcher'],
							message: 'Use typed callback props in Svelte 5 components.'
						},
						{
							name: '$app/stores',
							message: 'Use $app/state in SvelteKit 2.'
						}
					]
				}
			]
		}
	},
	{
		files: ['**/*.svelte', '**/*.svelte.ts', '**/*.svelte.js'],
		languageOptions: {
			parserOptions: {
				projectService: true,
				extraFileExtensions: ['.svelte'],
				parser: ts.parser,
				svelteConfig
			}
		},
		rules: {
			'svelte/button-has-type': 'error',
			'svelte/no-conflicting-module-names': 'error',
			'svelte/no-ignored-unsubscribe': 'error',
			'svelte/no-target-blank': 'error',
			'svelte/no-top-level-browser-globals': 'error',
			'svelte/prefer-derived-over-derived-by': 'error',
			'svelte/valid-compile': 'error',
			'svelte/valid-style-parse': 'error',
			'no-restricted-syntax': [
				'error',
				{
					selector: "ExportNamedDeclaration > VariableDeclaration[kind='let']",
					message: 'Use typed $props() instead of legacy export let component props.'
				},
				{
					selector: 'SvelteReactiveStatement',
					message: 'Use $derived or $effect instead of legacy $: reactivity.'
				},
				{
					selector: "SvelteDirective[kind='EventHandler']",
					message: 'Use event properties such as onclick instead of legacy on:event directives.'
				},
				{
					selector: "SvelteElement[name.name='slot']",
					message: 'Use typed snippets and {@render} instead of legacy slots.'
				}
			]
		}
	}
);
