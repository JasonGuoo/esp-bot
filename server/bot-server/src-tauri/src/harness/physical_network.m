#import <Foundation/Foundation.h>
#import <SystemConfiguration/SystemConfiguration.h>
#include <net/if.h>

int espbot_active_interface(char *name, size_t capacity) {
    @autoreleasepool {
        SCDynamicStoreRef store = SCDynamicStoreCreate(NULL, CFSTR("xiaodai-models"), NULL, NULL);
        if (!store) return 0;
        NSArray *patterns = @[@"Setup:/Network/Global/IPv4", @"Setup:/Network/Service/[^/]+/Interface",
                              @"State:/Network/Service/[^/]+/IPv[46]", @"State:/Network/Interface/[^/]+/Link"];
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
                    [device getCString:name maxLength:capacity encoding:NSASCIIStringEncoding] && if_nametoindex(name)) {
                    return 1;
                }
            }
        }
        return 0;
    }
}
