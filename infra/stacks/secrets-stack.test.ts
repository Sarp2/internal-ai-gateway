import { strictEqual } from 'node:assert';
import { test } from 'node:test';
import { App, RemovalPolicy } from 'aws-cdk-lib';
import { Match, Template } from 'aws-cdk-lib/assertions';
import { SecretsStack } from './secrets-stack.ts';

test('defines provider API key secrets', () => {
	const template = synthesizeTemplate();

	template.resourceCountIs('AWS::SecretsManager::Secret', 3);
	template.hasResourceProperties('AWS::SecretsManager::Secret', {
		Description: 'Anthropic provider API key for the internal AI gateway.',
	});
	template.hasResourceProperties('AWS::SecretsManager::Secret', {
		Description: 'OpenAI provider API key for the internal AI gateway.',
	});
	template.hasResourceProperties('AWS::SecretsManager::Secret', {
		Description: 'HMAC secret used to hash proxy API keys.',
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

test('generates a strong proxy api key hash secret', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::SecretsManager::Secret', {
		Description: 'HMAC secret used to hash proxy API keys.',
		GenerateSecretString: {
			PasswordLength: 64,
		},
	});
});

test('defines isolated integration secrets with production behavior', () => {
	const template = synthesizeIntegrationTemplate();

	template.resourceCountIs('AWS::SecretsManager::Secret', 3);
	template.hasResourceProperties('AWS::SecretsManager::Secret', {
		Description: 'Anthropic provider API key for the internal AI gateway.',
		Name: 'internal-ai-gateway/integration/anthropic-api-key',
	});
	template.hasResourceProperties('AWS::SecretsManager::Secret', {
		Description: 'OpenAI provider API key for the internal AI gateway.',
		Name: 'internal-ai-gateway/integration/openai-api-key',
	});
	template.hasResourceProperties('AWS::SecretsManager::Secret', {
		Description: 'HMAC secret used to hash proxy API keys.',
		GenerateSecretString: {
			PasswordLength: 64,
		},
		Name: 'internal-ai-gateway/integration/proxy-api-key-hash',
	});
});

test('destroys integration secrets with their stack', () => {
	const secrets = synthesizeIntegrationTemplate().findResources('AWS::SecretsManager::Secret');

	for (const secret of Object.values(secrets)) {
		strictEqual(secret.DeletionPolicy, 'Delete');
		strictEqual(secret.UpdateReplacePolicy, 'Delete');
	}
});

function synthesizeTemplate(): Template {
	const app = new App();
	const stack = new SecretsStack(app, 'TestSecretsStack');

	return Template.fromStack(stack);
}

function synthesizeIntegrationTemplate(): Template {
	const app = new App();
	const stack = new SecretsStack(app, 'TestIntegrationSecretsStack', {
		removalPolicy: RemovalPolicy.DESTROY,
		secretNamePrefix: 'internal-ai-gateway/integration',
	});

	return Template.fromStack(stack);
}
