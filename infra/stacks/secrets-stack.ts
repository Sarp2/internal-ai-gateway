import type { StackProps } from 'aws-cdk-lib';
import { RemovalPolicy, Stack } from 'aws-cdk-lib';
import { Secret } from 'aws-cdk-lib/aws-secretsmanager';
import type { Construct } from 'constructs';

const defaultSecretNamePrefix = 'internal-ai-gateway';

type SecretsStackProps = StackProps & {
	removalPolicy?: RemovalPolicy;
	secretNamePrefix?: string;
};

export class SecretsStack extends Stack {
	public readonly anthropicApiKeySecret: Secret;
	public readonly openAiApiKeySecret: Secret;
	public readonly proxyApiKeyHashSecret: Secret;

	public constructor(scope: Construct, id: string, props?: SecretsStackProps) {
		super(scope, id, props);
		const removalPolicy = props?.removalPolicy ?? RemovalPolicy.RETAIN;
		const secretNamePrefix = props?.secretNamePrefix ?? defaultSecretNamePrefix;

		this.anthropicApiKeySecret = new Secret(this, 'AnthropicApiKeySecret', {
			description: 'Anthropic provider API key for the internal AI gateway.',
			removalPolicy,
			secretName: `${secretNamePrefix}/anthropic-api-key`,
		});

		this.openAiApiKeySecret = new Secret(this, 'OpenAiApiKeySecret', {
			description: 'OpenAI provider API key for the internal AI gateway.',
			removalPolicy,
			secretName: `${secretNamePrefix}/openai-api-key`,
		});

		this.proxyApiKeyHashSecret = new Secret(this, 'ProxyApiKeyHashSecret', {
			description: 'HMAC secret used to hash proxy API keys.',
			generateSecretString: {
				passwordLength: 64,
			},
			removalPolicy,
			secretName: `${secretNamePrefix}/proxy-api-key-hash`,
		});
	}
}
