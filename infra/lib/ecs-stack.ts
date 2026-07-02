import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Duration, Stack, type StackProps } from 'aws-cdk-lib';
import type { IVpc, Vpc } from 'aws-cdk-lib/aws-ec2';
import { Peer, Port, SecurityGroup, SubnetType } from 'aws-cdk-lib/aws-ec2';
import {
	Cluster,
	ContainerImage,
	ContainerInsights,
	FargateService,
	FargateTaskDefinition,
	type ICluster,
	LogDrivers,
} from 'aws-cdk-lib/aws-ecs';
import {
	ApplicationLoadBalancer,
	ApplicationProtocol,
	type ApplicationTargetGroup,
} from 'aws-cdk-lib/aws-elasticloadbalancingv2';
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
	public readonly proxyLoadBalancer: ApplicationLoadBalancer;
	public readonly proxyLoadBalancerSecurityGroup: SecurityGroup;
	public readonly proxyTargetGroup: ApplicationTargetGroup;
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
				PORT: String(proxyContainerPort),
				RUST_LOG: 'info',
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

		this.proxyLoadBalancerSecurityGroup = new SecurityGroup(
			this,
			'ProxyLoadBalancerSecurityGroup',
			{
				vpc: props.vpc as IVpc,
				description: 'Allows public HTTP traffic to reach the proxy load balancer.',
				allowAllOutbound: true,
			},
		);

		this.proxyLoadBalancerSecurityGroup.addIngressRule(
			Peer.anyIpv4(),
			Port.tcp(80),
			'Allow public HTTP traffic.',
		);

		this.proxyServiceSecurityGroup.addIngressRule(
			this.proxyLoadBalancerSecurityGroup,
			Port.tcp(proxyContainerPort),
			'Allow proxy traffic from the load balancer.',
		);

		this.proxyLoadBalancer = new ApplicationLoadBalancer(this, 'ProxyLoadBalancer', {
			vpc: props.vpc as IVpc,
			internetFacing: true,
			securityGroup: this.proxyLoadBalancerSecurityGroup,
			vpcSubnets: {
				subnetType: SubnetType.PUBLIC,
			},
		});

		const proxyListener = this.proxyLoadBalancer.addListener('ProxyHttpListener', {
			port: 80,
			protocol: ApplicationProtocol.HTTP,
			open: false,
		});

		this.proxyTargetGroup = proxyListener.addTargets('ProxyTargets', {
			port: proxyContainerPort,
			protocol: ApplicationProtocol.HTTP,
			targets: [
				this.proxyService.loadBalancerTarget({
					containerName: 'proxy',
					containerPort: proxyContainerPort,
				}),
			],
			healthCheck: {
				path: '/health',
				healthyHttpCodes: '200',
				interval: Duration.seconds(30),
				timeout: Duration.seconds(5),
				healthyThresholdCount: 2,
				unhealthyThresholdCount: 3,
			},
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
