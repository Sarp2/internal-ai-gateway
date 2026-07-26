import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Duration, RemovalPolicy, Stack, type StackProps } from 'aws-cdk-lib';
import type { IVpc, Vpc } from 'aws-cdk-lib/aws-ec2';
import { Port, SecurityGroup, SubnetType } from 'aws-cdk-lib/aws-ec2';
import {
	type Cluster,
	ContainerImage,
	FargateService,
	FargateTaskDefinition,
	type ICluster,
	LogDrivers,
} from 'aws-cdk-lib/aws-ecs';
import { LogGroup, RetentionDays } from 'aws-cdk-lib/aws-logs';
import {
	DnsRecordType,
	type IPrivateDnsNamespace,
	type PrivateDnsNamespace,
} from 'aws-cdk-lib/aws-servicediscovery';
import type { Construct } from 'constructs';

const currentDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = join(currentDirectory, '..', '..');
const providerMockContainerPort = 8080;
const anthropicProviderMockName = 'anthropic-provider-mock';
const openAiProviderMockName = 'openai-provider-mock';
const providerMockAssetExcludes = [
	'.git',
	'cdk.out',
	'dist',
	'infra/cdk.out',
	'node_modules',
	'services/provider-mock/target',
	'services/proxy/target',
	'target',
];

type ProviderMockStackProps = StackProps & {
	cluster: Cluster;
	namespace: PrivateDnsNamespace;
	proxySecurityGroup: SecurityGroup;
	vpc: Vpc;
};

type ProviderMockConfiguration = {
	displayName: string;
	idPrefix: string;
	logName: string;
	serviceName: string;
};

type ProviderMockResources = {
	service: FargateService;
	securityGroup: SecurityGroup;
	taskDefinition: FargateTaskDefinition;
};

export class ProviderMockStack extends Stack {
	public readonly anthropicService: FargateService;
	public readonly anthropicServiceSecurityGroup: SecurityGroup;
	public readonly anthropicTaskDefinition: FargateTaskDefinition;
	public readonly openAiService: FargateService;
	public readonly openAiServiceSecurityGroup: SecurityGroup;
	public readonly openAiTaskDefinition: FargateTaskDefinition;

	public constructor(scope: Construct, id: string, props: ProviderMockStackProps) {
		super(scope, id, props);

		const anthropic = this.createProviderMock(props, {
			displayName: 'Anthropic',
			idPrefix: 'AnthropicProviderMock',
			logName: 'anthropic',
			serviceName: anthropicProviderMockName,
		});
		this.anthropicService = anthropic.service;
		this.anthropicServiceSecurityGroup = anthropic.securityGroup;
		this.anthropicTaskDefinition = anthropic.taskDefinition;

		const openAi = this.createProviderMock(props, {
			displayName: 'OpenAI',
			idPrefix: 'OpenAiProviderMock',
			logName: 'openai',
			serviceName: openAiProviderMockName,
		});
		this.openAiService = openAi.service;
		this.openAiServiceSecurityGroup = openAi.securityGroup;
		this.openAiTaskDefinition = openAi.taskDefinition;
	}

	private createProviderMock(
		props: ProviderMockStackProps,
		configuration: ProviderMockConfiguration,
	): ProviderMockResources {
		const logGroup = new LogGroup(this, `${configuration.idPrefix}LogGroup`, {
			logGroupName: `/internal-ai-gateway/integration/provider-mocks/${configuration.logName}`,
			removalPolicy: RemovalPolicy.DESTROY,
			retention: RetentionDays.ONE_WEEK,
		});

		const taskDefinition = new FargateTaskDefinition(
			this,
			`${configuration.idPrefix}TaskDefinition`,
			{
				cpu: 256,
				family: `internal-ai-gateway-integration-${configuration.serviceName}`,
				memoryLimitMiB: 512,
			},
		);

		taskDefinition.addContainer(`${configuration.idPrefix}Container`, {
			containerName: configuration.serviceName,
			essential: true,
			environment: {
				PORT: String(providerMockContainerPort),
			},
			healthCheck: {
				command: [
					'CMD-SHELL',
					`wget -q -O - http://127.0.0.1:${providerMockContainerPort}/health | grep -q '"status":"healthy"'`,
				],
				interval: Duration.seconds(30),
				retries: 3,
				startPeriod: Duration.seconds(10),
				timeout: Duration.seconds(5),
			},
			image: ContainerImage.fromAsset(repositoryRoot, {
				exclude: providerMockAssetExcludes,
				file: 'services/provider-mock/Dockerfile',
			}),
			logging: LogDrivers.awsLogs({
				logGroup,
				streamPrefix: configuration.serviceName,
			}),
			portMappings: [
				{
					containerPort: providerMockContainerPort,
				},
			],
		});

		const securityGroup = new SecurityGroup(this, `${configuration.idPrefix}SecurityGroup`, {
			allowAllOutbound: true,
			description: `Allows the integration proxy to reach the ${configuration.displayName} provider mock.`,
			vpc: props.vpc as IVpc,
		});
		securityGroup.addIngressRule(
			props.proxySecurityGroup,
			Port.tcp(providerMockContainerPort),
			`Allow ${configuration.displayName} mock requests from the integration proxy.`,
		);

		const service = new FargateService(this, `${configuration.idPrefix}Service`, {
			assignPublicIp: false,
			circuitBreaker: {
				rollback: true,
			},
			cloudMapOptions: {
				cloudMapNamespace: props.namespace as IPrivateDnsNamespace,
				dnsRecordType: DnsRecordType.A,
				dnsTtl: Duration.seconds(10),
				name: configuration.serviceName,
			},
			cluster: props.cluster as ICluster,
			desiredCount: 1,
			enableECSManagedTags: true,
			maxHealthyPercent: 200,
			minHealthyPercent: 100,
			securityGroups: [securityGroup],
			taskDefinition,
			vpcSubnets: {
				subnetType: SubnetType.PRIVATE_WITH_EGRESS,
			},
		});

		return { service, securityGroup, taskDefinition };
	}
}
