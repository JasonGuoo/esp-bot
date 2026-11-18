#import <Foundation/Foundation.h>
#import <SystemConfiguration/SystemConfiguration.h>
#include <net/if.h>

unsigned int espbot_active_interface(char *name, size_t capacity, char *dns, size_t dns_capacity) {
    @autoreleasepool {
        SCDynamicStoreRef store = SCDynamicStoreCreate(NULL, CFSTR("xiaodai-models"), NULL, NULL);
        if (!store) return 0;
        NSArray *patterns = @[@"Setup:/Network/Global/IPv4", @"Setup:/Network/Service/[^/]+/Interface",
                              @"State:/Network/Service/[^/]+/IPv[46]", @"State:/Network/Service/[^/]+/DNS", @"State:/Network/Interface/[^/]+/Link"];
        NSDictionary *snapshot = CFBridgingRelease(SCDynamicStoreCopyMultiple(store, NULL, (__bridge CFArrayRef)patterns));
        CFRelease(store);
        for (NSString *service in snapshot[@"Setup:/Network/Global/IPv4"][@"ServiceOrder"]) {
            NSDictionary *setup = snapshot[[NSString stringWithFormat:@"Setup:/Network/Service/%@/Interface", service]];
            NSString *device = setup[@"DeviceName"];
            if (!device || (![setup[@"Hardware"] isEqual:@"AirPort"] && ![setup[@"Hardware"] isEqual:@"Ethernet"])) continue;
            NSNumber *active = snapshot[[NSString stringWithFormat:@"State:/Network/Interface/%@/Link", device]][@"Active"];
            if (active && !active.boolValue) continue;
            for (NSString *family in @[@"IPv4", @"IPv6"]) {
                NSDictionary *state = snapshot[[NSString stringWithFormat:@"State:/Network/Service/%@/%@", service, family]];
                if ([device isEqual:state[@"InterfaceName"]] && state[@"Router"] && [state[@"Addresses"] count] &&
                    [device getCString:name maxLength:capacity encoding:NSASCIIStringEncoding]) {
                    unsigned int index = if_nametoindex(name);
                    if (!index) continue;
                    NSArray *servers = snapshot[[NSString stringWithFormat:@"State:/Network/Service/%@/DNS", service]][@"ServerAddresses"];
                    dns[0] = '\0';
                    if (servers.count) {
                        NSArray *first = [servers subarrayWithRange:NSMakeRange(0, MIN(servers.count, 3))];
                        if (![[first componentsJoinedByString:@","] getCString:dns maxLength:dns_capacity encoding:NSASCIIStringEncoding])
                            dns[0] = '\0';
                    }
                    return index;
                }
            }
        }
        return 0;
    }
}
