import { strictEqual } from 'node:assert';
import { test } from 'node:test';
import { App } from 'aws-cdk-lib';
import { Match, Template } from 'aws-cdk-lib/assertions';
import { SecretsStack } from './secrets-stack.ts';

test('defines provider API key secrets', () => {
	const template = synthesizeTemplate();

	template.resourceCountIs('AWS::SecretsManager::Secret', 2);
	template.hasResourceProperties('AWS::SecretsManager::Secret', {
		Description: 'Anthropic provider API key for the internal AI gateway.',
	});
	template.hasResourceProperties('AWS::SecretsManager::Secret', {
		Description: 'OpenAI provider API key for the internal AI gateway.',
	});
});

test('retains provider API key secrets when the stack is deleted', () => {
	const template = synthesizeTemplate();
	const secrets = template.findResources('AWS::SecretsManager::Secret');

	for (const secret of Object.values(secrets)) {
		strictEqual(secret.DeletionPolicy, 'Retain');
		strictEqual(secret.UpdateReplacePolicy, 'Retain');
	}
});

test('generates secret values instead of hardcoding provider keys', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::SecretsManager::Secret', {
		GenerateSecretString: Match.anyValue(),
	});
});

function synthesizeTemplate(): Template {
	const app = new App();
	const stack = new SecretsStack(app, 'TestSecretsStack');

	return Template.fromStack(stack);
}
