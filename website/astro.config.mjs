// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightLinksValidator from 'starlight-links-validator';

// https://astro.build/config
export default defineConfig({
	site: 'https://jraavis.github.io',
	base: '/siderite/',
	integrations: [
		starlight({
			title: 'siderite',
			description:
				'A FastAPI-style Rust web framework with Pydantic-style validation and a Django-style ORM.',
			favicon: '/favicon.svg',
			logo: {
				light: './src/assets/logo-light.svg',
				dark: './src/assets/logo-dark.svg',
				alt: 'siderite',
			},
			social: [
				{ icon: 'github', label: 'GitHub', href: 'https://github.com/jraavis/siderite' },
			],
			editLink: {
				baseUrl: 'https://github.com/jraavis/siderite/edit/master/website/',
			},
			customCss: ['./src/styles/theme.css'],
			expressiveCode: {
				styleOverrides: { borderRadius: '6px' },
			},
			head: [
				{
					tag: 'meta',
					attrs: { name: 'theme-color', content: '#B5472A' },
				},
			],
			plugins: [
				starlightLinksValidator({
					errorOnRelativeLinks: true,
					errorOnLocalLinks: false,
					exclude: ['/siderite/api', '/siderite/api/**'],
				}),
			],
			sidebar: [
				{
					label: 'Start here',
					items: [
						{ label: 'Installation', slug: 'start/installation' },
						{ label: 'First application', slug: 'start/first-app' },
						{ label: 'Core concepts', slug: 'start/concepts' },
						{ label: 'Examples', slug: 'start/examples' },
					],
				},
				{
					label: 'Tutorials',
					items: [
						{ label: 'Hello World', slug: 'tutorials/hello-world' },
						{ label: 'Todo on SQLite', slug: 'tutorials/todo-sqlite' },
						{ label: 'Blog on PostgreSQL', slug: 'tutorials/blog-postgres' },
						{ label: 'Two databases', slug: 'tutorials/polyglot' },
						{ label: 'Todo on MongoDB', slug: 'tutorials/todo-mongo' },
					],
				},
				{
					label: 'Guides — HTTP',
					items: [
						{ label: 'Routing', slug: 'guides/http/routing' },
						{ label: 'Extractors and responses', slug: 'guides/http/extractors' },
						{ label: 'Validation', slug: 'guides/http/validation' },
						{ label: 'OpenAPI 3.1', slug: 'guides/http/openapi' },
						{ label: 'Middleware and lifespan', slug: 'guides/http/middleware' },
						{ label: 'Dependency injection', slug: 'guides/http/di' },
						{ label: 'Security', slug: 'guides/http/security' },
						{ label: 'Errors', slug: 'guides/http/errors' },
					],
				},
				{
					label: 'Guides — Data',
					items: [
						{ label: 'Models', slug: 'guides/data/models' },
						{ label: 'QuerySets', slug: 'guides/data/querysets' },
						{ label: 'Relations', slug: 'guides/data/relations' },
						{ label: 'Transactions', slug: 'guides/data/transactions' },
						{ label: 'Migrations', slug: 'guides/data/migrations' },
						{ label: 'Backends', slug: 'guides/data/backends' },
						{ label: 'Database routing', slug: 'guides/data/database-routing' },
						{ label: 'Signals', slug: 'guides/data/signals' },
					],
				},
				{
					label: 'Guides — Production',
					items: [
						{ label: 'Configuration', slug: 'guides/production/config' },
						{ label: 'Cache', slug: 'guides/production/cache' },
						{ label: 'Observability', slug: 'guides/production/observability' },
						{ label: 'CLI', slug: 'guides/production/cli' },
						{ label: 'Testing', slug: 'guides/production/testing' },
					],
				},
				{
					label: 'Reference',
					items: [
						{ label: 'Crate map', slug: 'reference/crates' },
						{ label: 'Prelude', slug: 'reference/prelude' },
						{ label: 'Field attributes', slug: 'reference/field-attributes' },
						{ label: 'QuerySet API', slug: 'reference/queryset' },
						{ label: 'Backend matrix', slug: 'reference/backend-matrix' },
						{ label: 'Pydantic v2 mapping', slug: 'reference/pydantic' },
						{ label: 'API rustdoc', slug: 'reference/rustdoc' },
					],
				},
				{
					label: 'Internals',
					items: [
						{ label: 'Architecture', slug: 'internals/architecture' },
						{ label: 'Typed field constants', slug: 'internals/typed-fields' },
						{ label: 'QueryPlan IR', slug: 'internals/query-plan' },
					],
				},
				{
					label: 'Contributing',
					items: [
						{ label: 'Development', slug: 'contributing/development' },
						{ label: 'Releasing', slug: 'contributing/releasing' },
						{ label: 'Benchmarks', slug: 'contributing/benchmarks' },
					],
				},
			],
		}),
	],
});
