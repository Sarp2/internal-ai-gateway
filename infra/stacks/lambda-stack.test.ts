import { test } from 'node:test';
import { App } from 'aws-cdk-lib';
import { Match, Template } from 'aws-cdk-lib/assertions';
import { LambdaStack } from './lambda-stack.ts';

test('defines Lambda functions for short non-proxy routes', () => {
	const template = synthesizeTemplate();

	template.resourceCountIs('AWS::Lambda::Function', 1);
	template.hasResourceProperties('AWS::Lambda::Function', {
		Runtime: 'nodejs24.x',
		Architectures: ['arm64'],
	});
});

test('defines engineer usage Lambda configuration', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::Lambda::Function', {
		Description: Match.stringLikeRegexp('Returns usage data'),
		Handler: 'index.handler',
		Timeout: 10,
		MemorySize: 128,
		TracingConfig: {
			Mode: 'Active',
		},
		Environment: {
			Variables: Match.objectLike({
				FUNCTION_AREA: 'engineer',
				NODE_OPTIONS: '--enable-source-maps',
			}),
		},
	});
});

function synthesizeTemplate(): Template {
	const app = new App();
	const stack = new LambdaStack(app, 'TestLambdaStack');

	return Template.fromStack(stack);
}
