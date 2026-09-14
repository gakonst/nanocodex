/* Verify a real Venus device can execute commands and return correct memory.
 * No display, model, network, or software-renderer fallback is involved. */
#include "check_spv.h"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <vulkan/vulkan.h>
#define CHECK(expr)                                                            \
  do {                                                                         \
    VkResult r = (expr);                                                       \
    if (r != VK_SUCCESS) {                                                     \
      fprintf(stderr, "%s: Vulkan error %d\n", #expr, r);                      \
      return 1;                                                                \
    }                                                                          \
  } while (0)
#define REQUIRE(expr, msg)                                                     \
  do {                                                                         \
    if (!(expr)) {                                                             \
      fputs(msg "\n", stderr);                                                 \
      return 1;                                                                \
    }                                                                          \
  } while (0)
int main(void) {
  VkInstance instance;
  VkApplicationInfo app = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
                           .pApplicationName = "nanocodex-gpu-check",
                           .apiVersion = VK_API_VERSION_1_1};
  VkInstanceCreateInfo ici = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
                              .pApplicationInfo = &app};
  CHECK(vkCreateInstance(&ici, NULL, &instance));
  uint32_t count = 0;
  CHECK(vkEnumeratePhysicalDevices(instance, &count, NULL));
  REQUIRE(count, "No Vulkan devices; install the Venus guest driver");
  VkPhysicalDevice *devices = calloc(count, sizeof(*devices));
  REQUIRE(devices, "Device allocation failed");
  CHECK(vkEnumeratePhysicalDevices(instance, &count, devices));
  VkPhysicalDevice physical = VK_NULL_HANDLE;
  VkPhysicalDeviceProperties props;
  for (uint32_t i = 0; i < count; i++) {
    vkGetPhysicalDeviceProperties(devices[i], &props);
    if (props.deviceType != VK_PHYSICAL_DEVICE_TYPE_CPU &&
        strstr(props.deviceName, "Virtio-GPU Venus")) {
      physical = devices[i];
      break;
    }
  }
  free(devices);
  REQUIRE(physical, "No hardware Venus device; refusing software fallback");
  uint32_t queues = 0;
  vkGetPhysicalDeviceQueueFamilyProperties(physical, &queues, NULL);
  VkQueueFamilyProperties *qp = calloc(queues, sizeof(*qp));
  REQUIRE(qp, "Queue allocation failed");
  vkGetPhysicalDeviceQueueFamilyProperties(physical, &queues, qp);
  uint32_t family = UINT32_MAX;
  for (uint32_t i = 0; i < queues; i++)
    if (qp[i].queueCount && (qp[i].queueFlags & VK_QUEUE_COMPUTE_BIT)) {
      family = i;
      break;
    }
  free(qp);
  REQUIRE(family != UINT32_MAX, "No compute-capable GPU queue");
  float priority = 1;
  VkDeviceQueueCreateInfo qci = {.sType =
                                     VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
                                 .queueFamilyIndex = family,
                                 .queueCount = 1,
                                 .pQueuePriorities = &priority};
  VkDeviceCreateInfo dci = {.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
                            .queueCreateInfoCount = 1,
                            .pQueueCreateInfos = &qci};
  VkDevice device;
  CHECK(vkCreateDevice(physical, &dci, NULL, &device));
  VkQueue queue;
  vkGetDeviceQueue(device, family, 0, &queue);
  const VkDeviceSize bytes = 65536 * sizeof(uint32_t);
  VkBuffer buffer;
  VkBufferCreateInfo bci = {.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
                            .size = bytes,
                            .usage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT,
                            .sharingMode = VK_SHARING_MODE_EXCLUSIVE};
  CHECK(vkCreateBuffer(device, &bci, NULL, &buffer));
  VkMemoryRequirements requirements;
  vkGetBufferMemoryRequirements(device, buffer, &requirements);
  VkPhysicalDeviceMemoryProperties memory;
  vkGetPhysicalDeviceMemoryProperties(physical, &memory);
  uint32_t type = UINT32_MAX;
  for (uint32_t i = 0; i < memory.memoryTypeCount; i++)
    if ((requirements.memoryTypeBits & (1u << i)) &&
        (memory.memoryTypes[i].propertyFlags &
         VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT)) {
      type = i;
      break;
    }
  REQUIRE(type != UINT32_MAX, "No host-visible GPU memory");
  VkMemoryAllocateInfo mai = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
                              .allocationSize = requirements.size,
                              .memoryTypeIndex = type};
  VkDeviceMemory allocation;
  CHECK(vkAllocateMemory(device, &mai, NULL, &allocation));
  CHECK(vkBindBufferMemory(device, buffer, allocation, 0));
  VkDescriptorSetLayoutBinding binding = {
      .binding = 0,
      .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
      .descriptorCount = 1,
      .stageFlags = VK_SHADER_STAGE_COMPUTE_BIT};
  VkDescriptorSetLayoutCreateInfo dlci = {
      .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
      .bindingCount = 1,
      .pBindings = &binding};
  VkDescriptorSetLayout descriptor_layout;
  CHECK(vkCreateDescriptorSetLayout(device, &dlci, NULL, &descriptor_layout));
  VkPipelineLayoutCreateInfo plci = {
      .sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
      .setLayoutCount = 1,
      .pSetLayouts = &descriptor_layout};
  VkPipelineLayout pipeline_layout;
  CHECK(vkCreatePipelineLayout(device, &plci, NULL, &pipeline_layout));
  VkShaderModuleCreateInfo smci = {
      .sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
      .codeSize = sizeof(check_spv),
      .pCode = check_spv};
  VkShaderModule shader;
  CHECK(vkCreateShaderModule(device, &smci, NULL, &shader));
  VkComputePipelineCreateInfo cpci = {
      .sType = VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
      .stage = {.sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
                .stage = VK_SHADER_STAGE_COMPUTE_BIT,
                .module = shader,
                .pName = "main"},
      .layout = pipeline_layout};
  VkPipeline pipeline;
  CHECK(vkCreateComputePipelines(device, VK_NULL_HANDLE, 1, &cpci, NULL,
                                 &pipeline));
  VkDescriptorPoolSize size = {.type = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
                               .descriptorCount = 1};
  VkDescriptorPoolCreateInfo dpci = {
      .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
      .maxSets = 1,
      .poolSizeCount = 1,
      .pPoolSizes = &size};
  VkDescriptorPool descriptor_pool;
  CHECK(vkCreateDescriptorPool(device, &dpci, NULL, &descriptor_pool));
  VkDescriptorSetAllocateInfo dsai = {
      .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
      .descriptorPool = descriptor_pool,
      .descriptorSetCount = 1,
      .pSetLayouts = &descriptor_layout};
  VkDescriptorSet descriptor;
  CHECK(vkAllocateDescriptorSets(device, &dsai, &descriptor));
  VkDescriptorBufferInfo buffer_info = {
      .buffer = buffer, .offset = 0, .range = bytes};
  VkWriteDescriptorSet write = {.sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
                                .dstSet = descriptor,
                                .dstBinding = 0,
                                .descriptorCount = 1,
                                .descriptorType =
                                    VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
                                .pBufferInfo = &buffer_info};
  vkUpdateDescriptorSets(device, 1, &write, 0, NULL);
  VkCommandPool pool;
  VkCommandPoolCreateInfo pci = {.sType =
                                     VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
                                 .queueFamilyIndex = family};
  CHECK(vkCreateCommandPool(device, &pci, NULL, &pool));
  VkCommandBuffer cmd;
  VkCommandBufferAllocateInfo cai = {
      .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
      .commandPool = pool,
      .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
      .commandBufferCount = 1};
  CHECK(vkAllocateCommandBuffers(device, &cai, &cmd));
  VkCommandBufferBeginInfo begin = {
      .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
      .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT};
  CHECK(vkBeginCommandBuffer(cmd, &begin));
  vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pipeline);
  vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pipeline_layout,
                          0, 1, &descriptor, 0, NULL);
  vkCmdDispatch(cmd, 65536 / 64, 1, 1);
  VkMemoryBarrier barrier = {.sType = VK_STRUCTURE_TYPE_MEMORY_BARRIER,
                             .srcAccessMask = VK_ACCESS_SHADER_WRITE_BIT,
                             .dstAccessMask = VK_ACCESS_HOST_READ_BIT};
  vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT,
                       VK_PIPELINE_STAGE_HOST_BIT, 0, 1, &barrier, 0, NULL, 0,
                       NULL);
  CHECK(vkEndCommandBuffer(cmd));
  VkFence fence;
  VkFenceCreateInfo fci = {.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
  CHECK(vkCreateFence(device, &fci, NULL, &fence));
  VkSubmitInfo submit = {.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
                         .commandBufferCount = 1,
                         .pCommandBuffers = &cmd};
  CHECK(vkQueueSubmit(queue, 1, &submit, fence));
  CHECK(vkWaitForFences(device, 1, &fence, VK_TRUE, 10000000000ull));
  void *mapped;
  CHECK(vkMapMemory(device, allocation, 0, VK_WHOLE_SIZE, 0, &mapped));
  VkMappedMemoryRange range = {.sType = VK_STRUCTURE_TYPE_MAPPED_MEMORY_RANGE,
                               .memory = allocation,
                               .offset = 0,
                               .size = VK_WHOLE_SIZE};
  CHECK(vkInvalidateMappedMemoryRanges(device, 1, &range));
  for (size_t i = 0; i < bytes / sizeof(uint32_t); i++)
    REQUIRE(((uint32_t *)mapped)[i] == i * 3 + 7,
            "GPU memory verification failed");
  printf("%s: GPU compute and %llu-byte readback verified (65536 values)\n",
         props.deviceName, (unsigned long long)bytes);
  vkUnmapMemory(device, allocation);
  vkDestroyFence(device, fence, NULL);
  vkDestroyCommandPool(device, pool, NULL);
  vkDestroyDescriptorPool(device, descriptor_pool, NULL);
  vkDestroyPipeline(device, pipeline, NULL);
  vkDestroyShaderModule(device, shader, NULL);
  vkDestroyPipelineLayout(device, pipeline_layout, NULL);
  vkDestroyDescriptorSetLayout(device, descriptor_layout, NULL);
  vkDestroyBuffer(device, buffer, NULL);
  vkFreeMemory(device, allocation, NULL);
  vkDestroyDevice(device, NULL);
  vkDestroyInstance(instance, NULL);
  return 0;
}
