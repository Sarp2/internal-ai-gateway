import { strictEqual } from 'node:assert';
import { test } from 'node:test';
import { App } from 'aws-cdk-lib';
import { Template } from 'aws-cdk-lib/assertions';
import { InternalAiGatewayStack } from './app-stack.ts';

test('synthesizes an empty CDK stack', () => {
	const app = new App();
	const stack = new InternalAiGatewayStack(app, 'TestStack');

	const template = Template.fromStack(stack).toJSON();

	strictEqual(Object.keys(template.Resources ?? {}).length, 0);
});
