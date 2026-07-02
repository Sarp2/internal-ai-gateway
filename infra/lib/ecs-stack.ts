import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { StackProps } from 'aws-cdk-lib';
import { Stack } from 'aws-cdk-lib';
import type { IVpc, Vpc } from 'aws-cdk-lib/aws-ec2';
import { SecurityGroup, SubnetType } from 'aws-cdk-lib/aws-ec2';
import {
	Cluster,
	ContainerImage,
	ContainerInsights,
	FargateService,
	FargateTaskDefinition,
	type ICluster,
	LogDrivers,
} from 'aws-cdk-lib/aws-ecs';
import { LogGroup, RetentionDays } from 'aws-cdk-lib/aws-logs';
import type { Construct } from 'constructs';

const currentDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = join(currentDirectory, '..', '..');
const proxyContainerPort = 8080;
const proxyDesiredTaskCount = 2;
const proxyMaxTaskCount = 10;
const proxyScaleTargetPercent = 60;

type EcsStackProps = StackProps & {
	vpc: Vpc;
};

export class EcsStack extends Stack {
	public readonly cluster: Cluster;
	public readonly proxyService: FargateService;
	public readonly proxyServiceSecurityGroup: SecurityGroup;
	public readonly proxyTaskDefinition: FargateTaskDefinition;
	public readonly proxyLogGroup: LogGroup;

	public constructor(scope: Construct, id: string, props: EcsStackProps) {
		super(scope, id, props);

		this.cluster = new Cluster(this, 'ProxyCluster', {
			// CDK's concrete Vpc is compatible with IVpc, but exactOptionalPropertyTypes
			// rejects one optional property in the upstream type relationship.
			vpc: props.vpc as IVpc,
			clusterName: 'internal-ai-gateway-proxy',
			enableFargateCapacityProviders: true,
			containerInsightsV2: ContainerInsights.ENHANCED,
		});

		this.proxyLogGroup = new LogGroup(this, 'ProxyLogGroup', {
			logGroupName: '/internal-ai-gateway/proxy',
			retention: RetentionDays.ONE_MONTH,
		});

		this.proxyTaskDefinition = new FargateTaskDefinition(this, 'ProxyTaskDefinition', {
			family: 'internal-ai-gateway-proxy',
			cpu: 512,
			memoryLimitMiB: 1024,
		});

		this.proxyTaskDefinition.addContainer('ProxyContainer', {
			containerName: 'proxy',
			image: ContainerImage.fromAsset(repositoryRoot, {
				exclude: ['.git', 'cdk.out', 'dist', 'infra/cdk.out', 'node_modules'],
				file: 'services/proxy/Dockerfile',
			}),
			essential: true,
			environment: {
				NODE_ENV: 'production',
				PORT: String(proxyContainerPort),
			},
			logging: LogDrivers.awsLogs({
				logGroup: this.proxyLogGroup,
				streamPrefix: 'proxy',
			}),
			portMappings: [
				{
					containerPort: proxyContainerPort,
				},
			],
		});

		this.proxyServiceSecurityGroup = new SecurityGroup(this, 'ProxyServiceSecurityGroup', {
			vpc: props.vpc as IVpc,
			description: 'Controls network access for proxy ECS tasks.',
			allowAllOutbound: true,
		});

		this.proxyService = new FargateService(this, 'ProxyService', {
			cluster: this.cluster as ICluster,
			taskDefinition: this.proxyTaskDefinition,
			serviceName: 'internal-ai-gateway-proxy',
			desiredCount: proxyDesiredTaskCount,
			assignPublicIp: false,
			vpcSubnets: {
				subnetType: SubnetType.PRIVATE_WITH_EGRESS,
			},
			securityGroups: [this.proxyServiceSecurityGroup],
			minHealthyPercent: 100,
			maxHealthyPercent: 200,
			circuitBreaker: {
				rollback: true,
			},
			enableECSManagedTags: true,
		});

		const proxyScaling = this.proxyService.autoScaleTaskCount({
			minCapacity: proxyDesiredTaskCount,
			maxCapacity: proxyMaxTaskCount,
		});

		proxyScaling.scaleOnCpuUtilization('ProxyCpuScaling', {
			targetUtilizationPercent: proxyScaleTargetPercent,
		});

		proxyScaling.scaleOnMemoryUtilization('ProxyMemoryScaling', {
			targetUtilizationPercent: proxyScaleTargetPercent,
		});
	}
}
